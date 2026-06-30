//! Shared-memory control bridge between the daemon (control plane) and the
//! Windows APO (audio plane).
//!
//! The daemon owns the authoritative `ProcessorChain` state. It serialises that
//! state into a fixed `#[repr(C)]` [`SharedState`] block backed by a
//! memory-mapped file. The APO, running inside `audiodg.exe`, maps the same
//! file read-only and rebuilds its chain whenever the generation counter
//! changes.
//!
//! Synchronisation is a seqlock: the writer bumps `generation` to an odd value
//! before writing the snapshot and to the next even value after. A reader
//! retries while the value is odd or changed mid-read. This is lock-free and
//! needs no shared OS mutex across the daemon/audiodg trust boundary — the only
//! shared resource is the file, guarded by normal filesystem ACLs.
//!
//! This module is platform-agnostic (used by the daemon on every OS for the
//! type definitions; the file is only actually produced/consumed on Windows).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use resonance_dsp::chain::{FxEffect, ProcessorChain};
use resonance_dsp::channel::{ChannelMask, ChannelMatrix};
use resonance_dsp::effects::Effect;
use resonance_dsp::filter::{ApoFilter, FilterType};

/// Maximum number of EQ bands carried in the shared block.
pub const MAX_FILTERS: usize = 32;
/// Max channel count a square routing matrix is carried for. Per-channel EQ
/// (masks) works at any channel count; only the remap matrix is capped (a 64×64
/// matrix would bloat every snapshot — 16 covers 7.1.4 and below).
pub const MAX_ROUTE: usize = 16;
/// `"RAPO"` little-endian — sanity tag for the shared block.
pub const STATE_MAGIC: u32 = 0x4F50_4152;
/// Layout version; bump on any `#[repr(C)]` change below.
/// v3: per-band channel mask + square routing matrix.
/// v4: + Loudness effect snapshot.
pub const STATE_VERSION: u32 = 4;

/// Number of spectrum bins carried in telemetry (matches the daemon's display).
pub const TELEMETRY_BINS: usize = 64;

/// One EQ band.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FilterSnapshot {
    /// `FilterType` discriminant (see [`filter_type_to_u32`]).
    pub kind: u32,
    /// `0` = bypassed, `1` = active.
    pub enabled: u32,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    /// `ChannelMask` bits — which channels this band applies to. `u64::MAX` (all
    /// bits) = global. Channel-count-independent, so it works on any APO format.
    pub channels: u64,
}

/// One FxSound-style effect.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EffectSnapshot {
    pub enabled: u32,
    pub intensity: f64,
}

/// The full chain state, minus the audio format (channels/sample-rate are owned
/// by the APO from the negotiated stream format, not dictated by the daemon).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ChainSnapshot {
    pub enabled: u32,
    pub preamp_db: f64,
    pub fidelity: EffectSnapshot,
    pub ambience: EffectSnapshot,
    pub surround: EffectSnapshot,
    pub dynamic_boost: EffectSnapshot,
    pub bass: EffectSnapshot,
    pub loudness: EffectSnapshot,
    pub num_filters: u32,
    pub filters: [FilterSnapshot; MAX_FILTERS],
    /// Square output routing matrix dimension: `0` = none/identity (passthrough);
    /// else `N` means an `N×N` remap stored row-major in the first `N*N` entries
    /// of `route_gains`. Only applied by the APO when `N` equals its live channel
    /// count (the in-graph filter is in-place, so remap must be square).
    pub route_channels: u32,
    _pad_route: u32,
    pub route_gains: [f64; MAX_ROUTE * MAX_ROUTE],
}

impl Default for ChainSnapshot {
    fn default() -> Self {
        Self {
            enabled: 1,
            preamp_db: 0.0,
            fidelity: EffectSnapshot::default(),
            ambience: EffectSnapshot::default(),
            surround: EffectSnapshot::default(),
            dynamic_boost: EffectSnapshot::default(),
            bass: EffectSnapshot::default(),
            loudness: EffectSnapshot::default(),
            num_filters: 0,
            filters: [FilterSnapshot::default(); MAX_FILTERS],
            route_channels: 0,
            _pad_route: 0,
            route_gains: [0.0; MAX_ROUTE * MAX_ROUTE],
        }
    }
}

/// Extract a square routing matrix from a chain into the snapshot's fixed array.
/// Returns `(dim, gains)`; `dim == 0` when there's no carriable routing (none,
/// identity, non-square, or wider than [`MAX_ROUTE`]).
fn route_snapshot(chain: &ProcessorChain) -> (u32, [f64; MAX_ROUTE * MAX_ROUTE]) {
    let mut gains = [0.0; MAX_ROUTE * MAX_ROUTE];
    match &chain.routing {
        Some(m)
            if m.in_ch() == m.out_ch()
                && m.in_ch() <= MAX_ROUTE
                && m.in_ch() > 0
                && !m.is_identity() =>
        {
            let d = m.in_ch();
            gains[..d * d].copy_from_slice(m.gains());
            (d as u32, gains)
        }
        _ => (0, gains),
    }
}

/// Build a [`ChannelMatrix`] from the snapshot's routing fields, but only when it
/// is square at `channels` (the APO's live width) — otherwise `None` (passthrough).
fn route_matrix(
    route_channels: u32,
    route_gains: &[f64; MAX_ROUTE * MAX_ROUTE],
    channels: usize,
) -> Option<ChannelMatrix> {
    let d = route_channels as usize;
    if d == 0 || d != channels || d > MAX_ROUTE {
        return None;
    }
    ChannelMatrix::new(d, d, route_gains[..d * d].to_vec())
}

/// APO → daemon telemetry (meters + spectrum). Written by the APO worker thread
/// under a seqlock, read by the daemon. Only produced while
/// `telemetry_enabled` is set (a client is watching).
#[repr(C)]
pub struct Telemetry {
    pub generation: AtomicU64,
    pub in_peak: f32,
    pub out_peak: f32,
    pub in_rms: f32,
    pub out_rms: f32,
    /// The APO's live locked sample rate in Hz (0 until `LockForProcess`). Lets
    /// the Windows control-plane daemon report the real endpoint rate in
    /// `status` — it has no audio backend of its own, so the APO is the only one
    /// that knows the rate. Repurposes a former padding `f32` (no layout change).
    pub sample_rate: f32,
    pub spectrum: [f32; TELEMETRY_BINS],
}

/// Plain copy of telemetry returned to readers.
#[derive(Clone, Copy)]
pub struct TelemetrySnapshot {
    pub in_peak: f32,
    pub out_peak: f32,
    pub in_rms: f32,
    pub out_rms: f32,
    pub sample_rate: f32,
    pub spectrum: [f32; TELEMETRY_BINS],
}

/// The memory-mapped block. Header (magic/version) + the chain snapshot the
/// daemon publishes (its own seqlock) + a `telemetry_enabled` gate the daemon
/// sets + the APO→daemon telemetry block.
#[repr(C)]
pub struct SharedState {
    pub magic: u32,
    pub version: u32,
    pub generation: AtomicU64,
    pub snapshot: ChainSnapshot,
    /// Set by the daemon (1) while a client is watching; the APO skips all
    /// metering/FFT work when 0 — zero added cost on the audio thread.
    pub telemetry_enabled: AtomicU32,
    _pad2: u32,
    pub telemetry: Telemetry,
}

/// Total mapped size.
pub const STATE_SIZE: usize = std::mem::size_of::<SharedState>();

fn effect(chain: &ProcessorChain, e: FxEffect) -> EffectSnapshot {
    let (intensity, enabled) = match e {
        FxEffect::Fidelity => (chain.fidelity.intensity(), chain.fidelity.enabled()),
        FxEffect::Ambience => (chain.ambience.intensity(), chain.ambience.enabled()),
        FxEffect::Surround => (chain.surround.intensity(), chain.surround.enabled()),
        FxEffect::DynamicBoost => (
            chain.dynamic_boost.intensity(),
            chain.dynamic_boost.enabled(),
        ),
        FxEffect::Bass => (chain.bass.intensity(), chain.bass.enabled()),
        FxEffect::Loudness => (chain.loudness.intensity(), chain.loudness.enabled()),
    };
    EffectSnapshot {
        enabled: u32::from(enabled),
        intensity,
    }
}

impl ChainSnapshot {
    /// Capture the daemon's current chain (format-independent parameters only).
    #[must_use]
    pub fn from_chain(chain: &ProcessorChain) -> Self {
        let mut filters = [FilterSnapshot::default(); MAX_FILTERS];
        let n = chain.filters.len().min(MAX_FILTERS);
        for (dst, f) in filters.iter_mut().zip(chain.filters.iter()).take(n) {
            *dst = FilterSnapshot {
                kind: filter_type_to_u32(f.filter_type),
                enabled: u32::from(f.enabled),
                freq: f.freq,
                gain_db: f.gain_db,
                q: f.q,
                channels: f.mask.bits(),
            };
        }
        let (route_channels, route_gains) = route_snapshot(chain);
        Self {
            enabled: u32::from(chain.enabled),
            preamp_db: chain.preamp_db,
            fidelity: effect(chain, FxEffect::Fidelity),
            ambience: effect(chain, FxEffect::Ambience),
            surround: effect(chain, FxEffect::Surround),
            dynamic_boost: effect(chain, FxEffect::DynamicBoost),
            bass: effect(chain, FxEffect::Bass),
            loudness: effect(chain, FxEffect::Loudness),
            num_filters: n as u32,
            filters,
            route_channels,
            _pad_route: 0,
            route_gains,
        }
    }

    /// Rebuild a `ProcessorChain` at the APO's negotiated format, applying these
    /// parameters. Filters that fail to build (bad coeffs) are skipped.
    #[must_use]
    pub fn build_chain(&self, channels: usize, sample_rate: f64) -> ProcessorChain {
        let mut builder = ProcessorChain::builder()
            .channels(channels)
            .sample_rate(sample_rate)
            .preamp_db(self.preamp_db);

        for f in self
            .filters
            .iter()
            .take(self.num_filters.min(MAX_FILTERS as u32) as usize)
        {
            if let Ok(filter) = ApoFilter::builder()
                .filter_type(filter_type_from_u32(f.kind))
                .freq(f.freq)
                .gain_db(f.gain_db)
                .q(f.q)
                .enabled(f.enabled != 0)
                .channels(channels)
                .sample_rate(sample_rate)
                .channel_mask(ChannelMask::from_bits(f.channels))
                .build()
            {
                builder = builder.add_filter(filter);
            }
        }

        let mut chain = builder.build();
        chain.routing = route_matrix(self.route_channels, &self.route_gains, channels);
        chain.enabled = self.enabled != 0;
        chain.set_effect_intensity(FxEffect::Fidelity, self.fidelity.intensity);
        chain.set_effect_enabled(FxEffect::Fidelity, self.fidelity.enabled != 0);
        chain.set_effect_intensity(FxEffect::Ambience, self.ambience.intensity);
        chain.set_effect_enabled(FxEffect::Ambience, self.ambience.enabled != 0);
        chain.set_effect_intensity(FxEffect::Surround, self.surround.intensity);
        chain.set_effect_enabled(FxEffect::Surround, self.surround.enabled != 0);
        chain.set_effect_intensity(FxEffect::DynamicBoost, self.dynamic_boost.intensity);
        chain.set_effect_enabled(FxEffect::DynamicBoost, self.dynamic_boost.enabled != 0);
        chain.set_effect_intensity(FxEffect::Bass, self.bass.intensity);
        chain.set_effect_enabled(FxEffect::Bass, self.bass.enabled != 0);
        chain.set_effect_intensity(FxEffect::Loudness, self.loudness.intensity);
        chain.set_effect_enabled(FxEffect::Loudness, self.loudness.enabled != 0);
        chain
    }

    /// Apply these parameters to an EXISTING chain in place, preserving filter
    /// and effect state so live edits (dragging EQ bands) don't reset biquad
    /// history → clicks. Returns `false` if the band structure changed (count
    /// differs) and the caller should rebuild instead.
    pub fn apply_to(&self, chain: &mut ProcessorChain, sample_rate: f64) -> bool {
        chain.enabled = self.enabled != 0;
        chain.preamp_db = self.preamp_db;
        chain.set_effect_intensity(FxEffect::Fidelity, self.fidelity.intensity);
        chain.set_effect_enabled(FxEffect::Fidelity, self.fidelity.enabled != 0);
        chain.set_effect_intensity(FxEffect::Ambience, self.ambience.intensity);
        chain.set_effect_enabled(FxEffect::Ambience, self.ambience.enabled != 0);
        chain.set_effect_intensity(FxEffect::Surround, self.surround.intensity);
        chain.set_effect_enabled(FxEffect::Surround, self.surround.enabled != 0);
        chain.set_effect_intensity(FxEffect::DynamicBoost, self.dynamic_boost.intensity);
        chain.set_effect_enabled(FxEffect::DynamicBoost, self.dynamic_boost.enabled != 0);
        chain.set_effect_intensity(FxEffect::Bass, self.bass.intensity);
        chain.set_effect_enabled(FxEffect::Bass, self.bass.enabled != 0);
        chain.set_effect_intensity(FxEffect::Loudness, self.loudness.intensity);
        chain.set_effect_enabled(FxEffect::Loudness, self.loudness.enabled != 0);

        // Routing is format-independent state on the chain; apply it in place at
        // the chain's live width (square-only; mismatched/absent → passthrough).
        chain.routing = route_matrix(self.route_channels, &self.route_gains, chain.channels);

        let n = self.num_filters.min(MAX_FILTERS as u32) as usize;
        if chain.filters.len() != n {
            return false; // band added/removed — rebuild
        }
        for (slot, f) in chain.filters.iter_mut().zip(self.filters.iter()).take(n) {
            // update() recomputes coefficients but keeps the biquad's running
            // state, so coefficient changes are click-free.
            let _ = slot.update(
                filter_type_from_u32(f.kind),
                f.freq,
                f.gain_db,
                f.q,
                sample_rate,
            );
            slot.enabled = f.enabled != 0;
            // Per-channel target is plain state (no coefficient/history impact).
            slot.mask = ChannelMask::from_bits(f.channels);
        }
        true
    }
}

/// Stable `FilterType` → `u32` mapping for the shared block (must round-trip).
#[must_use]
pub fn filter_type_to_u32(t: FilterType) -> u32 {
    match t {
        FilterType::Peaking => 0,
        FilterType::LowShelf => 1,
        FilterType::LowShelf12Db => 2,
        FilterType::LowShelfQ => 3,
        FilterType::HighShelf => 4,
        FilterType::HighShelf12Db => 5,
        FilterType::HighShelfQ => 6,
        FilterType::LowPass => 7,
        FilterType::LowPassQ => 8,
        FilterType::HighPass => 9,
        FilterType::HighPassQ => 10,
        FilterType::BandPass => 11,
        FilterType::Notch => 12,
        FilterType::AllPass => 13,
    }
}

/// Inverse of [`filter_type_to_u32`]; unknown values fall back to `Peaking`.
#[must_use]
pub fn filter_type_from_u32(v: u32) -> FilterType {
    match v {
        1 => FilterType::LowShelf,
        2 => FilterType::LowShelf12Db,
        3 => FilterType::LowShelfQ,
        4 => FilterType::HighShelf,
        5 => FilterType::HighShelf12Db,
        6 => FilterType::HighShelfQ,
        7 => FilterType::LowPass,
        8 => FilterType::LowPassQ,
        9 => FilterType::HighPass,
        10 => FilterType::HighPassQ,
        11 => FilterType::BandPass,
        12 => FilterType::Notch,
        13 => FilterType::AllPass,
        _ => FilterType::Peaking,
    }
}

/// Default shared-state file path. `%ProgramData%\Resonance\apo_state.bin` on
/// Windows (readable by `audiodg.exe`), a temp path elsewhere (tests only).
#[must_use]
pub fn default_state_path() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
        base.join("Resonance").join("apo_state.bin")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir().join("resonance-apo-state.bin")
    }
}

/// The shared file, mapped read-write by both the daemon and the APO.
///
/// Two independent seqlock regions: the daemon owns the chain `snapshot`
/// (writes / the APO reads), the APO owns `telemetry` (writes / the daemon
/// reads). The daemon owns the `telemetry_enabled` gate.
pub struct SharedFile {
    mmap: memmap2::MmapMut,
}

impl SharedFile {
    /// Open (creating if needed) the shared file at `path`, sized to
    /// [`STATE_SIZE`]. Initialises the header only if it isn't already valid, so
    /// it's safe regardless of which process opens first.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the parent directory cannot be created, the
    /// file cannot be opened, it cannot be sized to [`STATE_SIZE`], or the
    /// memory map fails.
    // mmap base is page-aligned, exceeding SharedState's alignment.
    #[allow(clippy::cast_ptr_alignment)]
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.set_len(STATE_SIZE as u64)?;
        // SAFETY: file is sized to STATE_SIZE and owned for the map's lifetime.
        let mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };
        let this = Self { mmap };
        // SAFETY: map is STATE_SIZE bytes; SharedState is repr(C).
        let st = unsafe { &*this.mmap.as_ptr().cast::<SharedState>() };
        if st.magic != STATE_MAGIC || st.version != STATE_VERSION {
            let st = unsafe { &mut *(this.mmap.as_ptr() as *mut SharedState) };
            st.magic = STATE_MAGIC;
            st.version = STATE_VERSION;
            st.generation.store(0, Ordering::Release);
            st.telemetry_enabled.store(0, Ordering::Release);
            st.telemetry.generation.store(0, Ordering::Release);
        }
        Ok(this)
    }

    /// Backwards-compatible alias used by the daemon.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] under the same conditions as [`Self::open`].
    pub fn create(path: &Path) -> io::Result<Self> {
        Self::open(path)
    }

    // mmap base is page-aligned, exceeding SharedState's alignment.
    #[allow(clippy::cast_ptr_alignment)]
    fn state(&self) -> &SharedState {
        // SAFETY: map is STATE_SIZE bytes; SharedState is repr(C).
        unsafe { &*self.mmap.as_ptr().cast::<SharedState>() }
    }

    // mmap base is page-aligned, exceeding SharedState's alignment.
    #[allow(clippy::mut_from_ref, clippy::cast_ptr_alignment)]
    fn state_mut(&self) -> &mut SharedState {
        // SAFETY: map is STATE_SIZE bytes; SharedState is repr(C). Field-level
        // seqlocks/atomics guard concurrent access across processes.
        unsafe { &mut *(self.mmap.as_ptr() as *mut SharedState) }
    }

    // ---- chain snapshot: daemon writes, APO reads ----

    /// Publish the daemon's current chain under the seqlock.
    pub fn publish(&mut self, chain: &ProcessorChain) {
        let snap = ChainSnapshot::from_chain(chain);
        let st = self.state_mut();
        let g = st.generation.load(Ordering::Relaxed);
        st.generation.store(g.wrapping_add(1), Ordering::Release); // odd: writing
        st.snapshot = snap;
        st.generation.store(g.wrapping_add(2), Ordering::Release); // even: done
    }

    /// Current chain generation (cheap; lets the APO poller skip unchanged state).
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.state().generation.load(Ordering::Acquire)
    }

    /// Read a consistent chain snapshot (seqlock retry), or `None`.
    #[must_use]
    pub fn read(&self) -> Option<ChainSnapshot> {
        let st = self.state();
        if st.magic != STATE_MAGIC || st.version != STATE_VERSION {
            return None;
        }
        for _ in 0..16 {
            let g1 = st.generation.load(Ordering::Acquire);
            if g1 & 1 != 0 {
                continue;
            }
            let snap = st.snapshot;
            let g2 = st.generation.load(Ordering::Acquire);
            if g1 == g2 {
                return Some(snap);
            }
        }
        None
    }

    // ---- telemetry gate: daemon sets, APO reads ----

    /// Daemon: enable/disable telemetry production (call only while a client is
    /// watching). When disabled the APO does no metering/FFT work.
    pub fn set_telemetry_enabled(&self, on: bool) {
        self.state()
            .telemetry_enabled
            .store(u32::from(on), Ordering::Release);
    }

    /// APO: is a client watching? Cheap atomic load on the audio thread.
    #[must_use]
    pub fn telemetry_enabled(&self) -> bool {
        self.state().telemetry_enabled.load(Ordering::Acquire) != 0
    }

    // ---- telemetry: APO writes, daemon reads ----

    /// APO worker thread: publish meters + spectrum under the telemetry seqlock.
    pub fn write_telemetry(
        &self,
        in_peak: f32,
        out_peak: f32,
        in_rms: f32,
        out_rms: f32,
        sample_rate: f32,
        spectrum: &[f32; TELEMETRY_BINS],
    ) {
        let t = &mut self.state_mut().telemetry;
        let g = t.generation.load(Ordering::Relaxed);
        t.generation.store(g.wrapping_add(1), Ordering::Release); // odd
        t.in_peak = in_peak;
        t.out_peak = out_peak;
        t.in_rms = in_rms;
        t.out_rms = out_rms;
        t.sample_rate = sample_rate;
        t.spectrum = *spectrum;
        t.generation.store(g.wrapping_add(2), Ordering::Release); // even
    }

    /// Daemon: read a consistent telemetry snapshot (seqlock retry), or `None`.
    #[must_use]
    pub fn read_telemetry(&self) -> Option<TelemetrySnapshot> {
        let t = &self.state().telemetry;
        for _ in 0..16 {
            let g1 = t.generation.load(Ordering::Acquire);
            if g1 & 1 != 0 {
                continue;
            }
            let snap = TelemetrySnapshot {
                in_peak: t.in_peak,
                out_peak: t.out_peak,
                in_rms: t.in_rms,
                out_rms: t.out_rms,
                sample_rate: t.sample_rate,
                spectrum: t.spectrum,
            };
            let g2 = t.generation.load(Ordering::Acquire);
            if g1 == g2 {
                return Some(snap);
            }
        }
        None
    }
}

/// Daemon-facing alias (the daemon historically referenced `ApoStateWriter`).
pub type ApoStateWriter = SharedFile;

// ── Fresh cross-process reads (Windows) ──────────────────────────────────────
//
// On Windows the daemon (session 1) and the APO (audiodg, LocalService, session
// 0) live in different sessions. A long-lived memory-mapped *view* of the shared
// file does NOT observe the other process's writes there — but the writes do
// land in the file (a plain read, or a freshly-mapped view, sees them). So every
// cross-process *reader* fetches the bytes fresh each poll instead of reading
// through a stale mapping. Writers keep their mapping (their stores reach the
// file). Off the RT thread, so the extra read is free.

#[inline]
fn read_u32(buf: &[u8], off: usize) -> Option<u32> {
    buf.get(off..off + 4)
        .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
}
#[inline]
fn read_u64(buf: &[u8], off: usize) -> Option<u64> {
    buf.get(off..off + 8)
        .map(|b| u64::from_ne_bytes(b.try_into().unwrap()))
}
#[inline]
fn read_f32(buf: &[u8], off: usize) -> Option<f32> {
    buf.get(off..off + 4)
        .map(|b| f32::from_ne_bytes(b.try_into().unwrap()))
}

/// Fresh read of the daemon's chain snapshot + telemetry gate (generation,
/// snapshot, `telemetry_enabled`). Seqlock: read twice and require an even,
/// unchanged generation. `None` while a write is in flight or the header is
/// not yet valid — the caller simply polls again.
#[must_use]
pub fn read_chain_fresh(path: &Path) -> Option<(u64, ChainSnapshot, bool)> {
    let ver_off = std::mem::offset_of!(SharedState, version);
    let gen_off = std::mem::offset_of!(SharedState, generation);
    let snap_off = std::mem::offset_of!(SharedState, snapshot);
    let gate_off = std::mem::offset_of!(SharedState, telemetry_enabled);
    for _ in 0..8 {
        let b1 = std::fs::read(path).ok()?;
        // Reject a stale-layout file: reinterpreting an old ChainSnapshot at the
        // new (larger) offsets would scramble the chain. The writer reinitialises
        // the header on a version bump, so this only skips the brief window before
        // the matching-build daemon republishes.
        if b1.len() < STATE_SIZE
            || read_u32(&b1, 0)? != STATE_MAGIC
            || read_u32(&b1, ver_off)? != STATE_VERSION
        {
            return None;
        }
        let g1 = read_u64(&b1, gen_off)?;
        if g1 & 1 != 0 {
            continue; // writer mid-update
        }
        // SAFETY: ChainSnapshot is repr(C) + Copy with no padding-sensitive
        // invariants; read unaligned from the heap buffer at its field offset.
        let snap =
            unsafe { std::ptr::read_unaligned(b1.as_ptr().add(snap_off).cast::<ChainSnapshot>()) };
        let gate = read_u32(&b1, gate_off)? != 0;
        // Confirm the generation didn't change across the copy (no torn read).
        let g2 = read_u64(&std::fs::read(path).ok()?, gen_off)?;
        if g1 == g2 {
            return Some((g1, snap, gate));
        }
    }
    None
}

/// Fresh read of the APO's telemetry block (meters + spectrum), used by the
/// daemon. Same seqlock-over-fresh-reads scheme as [`read_chain_fresh`].
#[must_use]
pub fn read_telemetry_fresh(path: &Path) -> Option<TelemetrySnapshot> {
    let tel_off = std::mem::offset_of!(SharedState, telemetry);
    let gen_off = tel_off + std::mem::offset_of!(Telemetry, generation);
    for _ in 0..8 {
        let b = std::fs::read(path).ok()?;
        if b.len() < STATE_SIZE || read_u32(&b, 0)? != STATE_MAGIC {
            return None;
        }
        let g1 = read_u64(&b, gen_off)?;
        if g1 & 1 != 0 {
            continue;
        }
        let mut spectrum = [0.0f32; TELEMETRY_BINS];
        let spec_off = tel_off + std::mem::offset_of!(Telemetry, spectrum);
        for (i, s) in spectrum.iter_mut().enumerate() {
            *s = read_f32(&b, spec_off + i * 4)?;
        }
        let snap = TelemetrySnapshot {
            in_peak: read_f32(&b, tel_off + std::mem::offset_of!(Telemetry, in_peak))?,
            out_peak: read_f32(&b, tel_off + std::mem::offset_of!(Telemetry, out_peak))?,
            in_rms: read_f32(&b, tel_off + std::mem::offset_of!(Telemetry, in_rms))?,
            out_rms: read_f32(&b, tel_off + std::mem::offset_of!(Telemetry, out_rms))?,
            sample_rate: read_f32(&b, tel_off + std::mem::offset_of!(Telemetry, sample_rate))?,
            spectrum,
        };
        let g2 = read_u64(&std::fs::read(path).ok()?, gen_off)?;
        if g1 == g2 {
            return Some(snap);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    // Asserts compare against exact stored/expected round-trip values.
    #![allow(clippy::float_cmp)]
    use super::*;
    use resonance_dsp::filter::{ApoFilter, FilterType};

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("resonance-apo-{tag}-{}.bin", std::process::id()))
    }

    #[test]
    fn snapshot_round_trips_through_file() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .preamp_db(-3.5)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::HighShelf)
                    .freq(8000.0)
                    .gain_db(4.0)
                    .q(0.9)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48000.0)
                    .build()
                    .unwrap(),
            )
            .build();
        chain.set_effect_intensity(FxEffect::Bass, 0.7);
        chain.set_effect_enabled(FxEffect::Bass, true);

        let path = temp_path("rt");
        let mut w = ApoStateWriter::create(&path).unwrap();
        w.publish(&chain);

        let r = SharedFile::open(&path).unwrap();
        let snap = r.read().unwrap();
        assert_eq!(snap.preamp_db, -3.5);
        assert_eq!(snap.num_filters, 1);
        assert_eq!(
            snap.filters[0].kind,
            filter_type_to_u32(FilterType::HighShelf)
        );
        assert!((snap.filters[0].freq - 8000.0).abs() < 1e-9);
        assert!((snap.bass.intensity - 0.7).abs() < 1e-9);
        assert_eq!(snap.bass.enabled, 1);

        // The APO-side rebuild applies the captured parameters.
        let rebuilt = snap.build_chain(2, 48000.0);
        assert_eq!(rebuilt.filters.len(), 1);
        assert!((rebuilt.preamp_db + 3.5).abs() < 1e-9);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seqlock_generation_advances_even() {
        let path = temp_path("gen");
        let chain = ProcessorChain::builder().build();
        let mut w = ApoStateWriter::create(&path).unwrap();
        let r = SharedFile::open(&path).unwrap();

        let g0 = r.generation();
        assert_eq!(g0 % 2, 0);
        w.publish(&chain);
        let g1 = r.generation();
        assert_eq!(g1, g0 + 2);
        assert!(r.read().is_some());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mask_and_routing_round_trip_through_snapshot() {
        use resonance_dsp::channel::{ChannelMask, ChannelMatrix};

        let band = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(1000.0)
            .gain_db(6.0)
            .q(1.0)
            .enabled(true)
            .channels(2)
            .sample_rate(48000.0)
            .channel_mask(ChannelMask::single(0))
            .build()
            .unwrap();
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(band)
            .build();
        chain.routing = Some(ChannelMatrix::swap(2, 0, 1));

        let snap = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap.filters[0].channels, ChannelMask::single(0).bits());
        assert_eq!(snap.route_channels, 2);

        // Rebuild at the matching width: both the mask and the routing apply.
        let rebuilt = snap.build_chain(2, 48000.0);
        assert_eq!(rebuilt.filters.len(), 1);
        assert_eq!(rebuilt.filters[0].mask, ChannelMask::single(0));
        let r = rebuilt
            .routing
            .expect("square routing should apply at width 2");
        assert_eq!(r.out_ch(), 2);
        assert!(!r.is_identity());

        // Rebuild at a DIFFERENT width: the 2×2 routing must NOT apply (it would
        // misframe), but the channel-count-independent band mask still does.
        let rebuilt6 = snap.build_chain(6, 48000.0);
        assert!(
            rebuilt6.routing.is_none(),
            "square routing must match the live channel count"
        );
        assert_eq!(rebuilt6.filters[0].mask, ChannelMask::single(0));
    }

    #[test]
    fn filter_type_mapping_round_trips() {
        for t in [
            FilterType::Peaking,
            FilterType::LowShelf,
            FilterType::LowShelf12Db,
            FilterType::LowShelfQ,
            FilterType::HighShelf,
            FilterType::HighShelf12Db,
            FilterType::HighShelfQ,
            FilterType::LowPass,
            FilterType::LowPassQ,
            FilterType::HighPass,
            FilterType::HighPassQ,
            FilterType::BandPass,
            FilterType::Notch,
            FilterType::AllPass,
        ] {
            assert_eq!(filter_type_from_u32(filter_type_to_u32(t)), t);
        }
    }

    // ── routing snapshot carry/gate (cross-platform; protects the Windows APO) ──

    use resonance_dsp::channel::{ChannelMask, ChannelMatrix};

    fn chain_with_routing(channels: usize, m: Option<ChannelMatrix>) -> ProcessorChain {
        let mut c = ProcessorChain::builder()
            .channels(channels)
            .sample_rate(48000.0)
            .build();
        c.routing = m;
        c
    }

    #[test]
    fn route_snapshot_rejects_non_square() {
        // 2→4 upmix is not square → not carried (the in-place APO can't change width).
        let c = chain_with_routing(2, ChannelMatrix::new(2, 4, vec![0.0; 8]));
        assert_eq!(route_snapshot(&c).0, 0);
    }

    #[test]
    fn route_snapshot_skips_identity_and_none() {
        assert_eq!(route_snapshot(&chain_with_routing(4, None)).0, 0);
        assert_eq!(
            route_snapshot(&chain_with_routing(4, Some(ChannelMatrix::identity(4)))).0,
            0,
            "identity is implicit passthrough — not carried"
        );
    }

    #[test]
    fn route_snapshot_caps_at_max_route() {
        // 16×16 (boundary) carried; 17×17 dropped.
        assert_eq!(
            route_snapshot(&chain_with_routing(16, Some(ChannelMatrix::swap(16, 0, 1)))).0,
            16
        );
        assert_eq!(
            route_snapshot(&chain_with_routing(17, Some(ChannelMatrix::swap(17, 0, 1)))).0,
            0,
            "wider than MAX_ROUTE must not be carried"
        );
    }

    #[test]
    fn route_snapshot_carries_valid_square_dims_exactly() {
        for d in [1usize, 3, 4, 8, 16] {
            let m = ChannelMatrix::swap(d, 0, d.min(2) - 1);
            let c = chain_with_routing(d, Some(m.clone()));
            let (dim, gains) = route_snapshot(&c);
            if m.is_identity() {
                assert_eq!(dim, 0, "d={d}: identity skipped");
            } else {
                assert_eq!(dim as usize, d, "d={d}: dim carried");
                assert_eq!(&gains[..d * d], m.gains(), "d={d}: gains exact");
            }
        }
    }

    #[test]
    fn route_matrix_only_applies_square_at_live_width() {
        let m = ChannelMatrix::swap(4, 0, 1);
        let (_dim, gains) = route_snapshot(&chain_with_routing(4, Some(m)));
        // Matching width → Some; mismatched width / zero / oversized → None.
        assert!(route_matrix(4, &gains, 4).is_some());
        assert!(route_matrix(4, &gains, 6).is_none(), "dim != live width");
        assert!(route_matrix(0, &gains, 0).is_none());
        assert!(route_matrix(17, &gains, 17).is_none(), "> MAX_ROUTE");
    }

    // ── ChainSnapshot: masks, truncation, effects, apply_to ────────────────────

    fn peaking(freq: f64, gain: f64, channels: usize, mask: ChannelMask) -> ApoFilter {
        ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(freq)
            .gain_db(gain)
            .q(1.0)
            .enabled(true)
            .channels(channels)
            .sample_rate(48000.0)
            .channel_mask(mask)
            .build()
            .unwrap()
    }

    #[test]
    fn from_chain_captures_per_band_masks() {
        let masks = [
            ChannelMask::single(0),
            ChannelMask::single(1),
            ChannelMask::from_indices([0, 1]),
            ChannelMask::ALL,
        ];
        let mut b = ProcessorChain::builder().channels(2).sample_rate(48000.0);
        for (i, m) in masks.iter().enumerate() {
            b = b.add_filter(peaking(500.0 * (i as f64 + 1.0), 3.0, 2, *m));
        }
        let snap = ChainSnapshot::from_chain(&b.build());
        assert_eq!(snap.num_filters, 4);
        for (i, m) in masks.iter().enumerate() {
            assert_eq!(snap.filters[i].channels, m.bits(), "mask {i}");
        }
    }

    #[test]
    fn from_chain_truncates_at_max_filters() {
        let mut b = ProcessorChain::builder().channels(2).sample_rate(48000.0);
        for i in 0..(MAX_FILTERS + 8) {
            b = b.add_filter(peaking(100.0 + i as f64, 1.0, 2, ChannelMask::ALL));
        }
        let snap = ChainSnapshot::from_chain(&b.build());
        assert_eq!(snap.num_filters as usize, MAX_FILTERS);
        assert_eq!(snap.build_chain(2, 48000.0).filters.len(), MAX_FILTERS);
    }

    #[test]
    fn from_chain_captures_all_effects() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .build();
        chain.set_effect_intensity(FxEffect::Fidelity, 0.3);
        chain.set_effect_enabled(FxEffect::Fidelity, true);
        chain.set_effect_intensity(FxEffect::Bass, -0.5);
        chain.set_effect_enabled(FxEffect::Bass, true);
        chain.set_effect_enabled(FxEffect::DynamicBoost, false);
        let s = ChainSnapshot::from_chain(&chain);
        assert!((s.fidelity.intensity - 0.3).abs() < 1e-9 && s.fidelity.enabled == 1);
        assert!((s.bass.intensity + 0.5).abs() < 1e-9 && s.bass.enabled == 1);
        assert_eq!(s.dynamic_boost.enabled, 0);
    }

    #[test]
    fn apply_to_updates_masks_and_routing_in_place() {
        // Live chain: 2 global bands, no routing.
        let mut live = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(peaking(1000.0, 3.0, 2, ChannelMask::ALL))
            .add_filter(peaking(2000.0, 3.0, 2, ChannelMask::ALL))
            .build();
        // Snapshot from a chain with per-channel masks + a swap routing.
        let mut src = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(peaking(1000.0, 3.0, 2, ChannelMask::single(0)))
            .add_filter(peaking(2000.0, 3.0, 2, ChannelMask::single(1)))
            .build();
        src.routing = Some(ChannelMatrix::swap(2, 0, 1));
        let snap = ChainSnapshot::from_chain(&src);

        assert!(
            snap.apply_to(&mut live, 48000.0),
            "same band count → in-place"
        );
        assert_eq!(live.filters[0].mask, ChannelMask::single(0));
        assert_eq!(live.filters[1].mask, ChannelMask::single(1));
        assert!(live.routing.is_some(), "routing applied in place");
        assert!(!live.routing.unwrap().is_identity());
    }

    #[test]
    fn apply_to_signals_rebuild_on_band_count_change() {
        let mut live = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(peaking(1000.0, 3.0, 2, ChannelMask::ALL))
            .add_filter(peaking(2000.0, 3.0, 2, ChannelMask::ALL))
            .build();
        let snap = ChainSnapshot::from_chain(
            &ProcessorChain::builder()
                .channels(2)
                .sample_rate(48000.0)
                .add_filter(peaking(1000.0, 3.0, 2, ChannelMask::ALL))
                .build(),
        );
        assert!(!snap.apply_to(&mut live, 48000.0), "1 vs 2 bands → rebuild");
        assert_eq!(live.filters.len(), 2, "chain untouched on rebuild signal");
    }

    // ── SharedFile + fresh-read seqlock / version guard ────────────────────────

    fn patch_u32(path: &std::path::Path, offset: usize, val: u32) {
        let mut b = std::fs::read(path).unwrap();
        b[offset..offset + 4].copy_from_slice(&val.to_ne_bytes());
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn read_chain_fresh_rejects_old_version_and_bad_magic() {
        let path = temp_path("ver");
        {
            let mut w = ApoStateWriter::create(&path).unwrap();
            w.publish(
                &ProcessorChain::builder()
                    .channels(2)
                    .sample_rate(48000.0)
                    .build(),
            );
        }
        assert!(read_chain_fresh(&path).is_some(), "valid file reads");

        // Stale layout: a v2 file must be refused (its ChainSnapshot is smaller).
        patch_u32(&path, std::mem::offset_of!(SharedState, version), 2);
        assert!(read_chain_fresh(&path).is_none(), "old version rejected");

        // Bad magic refused too.
        patch_u32(
            &path,
            std::mem::offset_of!(SharedState, version),
            STATE_VERSION,
        );
        patch_u32(&path, 0, 0xDEAD_BEEF);
        assert!(read_chain_fresh(&path).is_none(), "bad magic rejected");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_chain_fresh_returns_gate() {
        let path = temp_path("gate");
        let w = ApoStateWriter::create(&path).unwrap();
        let mut w = w;
        w.publish(
            &ProcessorChain::builder()
                .channels(2)
                .sample_rate(48000.0)
                .build(),
        );
        w.set_telemetry_enabled(true);
        let (_g, _snap, gate) = read_chain_fresh(&path).expect("reads");
        assert!(gate, "telemetry gate reflected in fresh read");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn telemetry_round_trips_via_file_and_fresh() {
        let path = temp_path("tel");
        let w = ApoStateWriter::create(&path).unwrap();
        let mut spectrum = [0.0f32; TELEMETRY_BINS];
        for (i, s) in spectrum.iter_mut().enumerate() {
            *s = i as f32 / TELEMETRY_BINS as f32;
        }
        w.write_telemetry(0.8, 0.6, 0.3, 0.25, 96000.0, &spectrum);

        let via_map = w.read_telemetry().expect("mapped read");
        assert!((via_map.in_peak - 0.8).abs() < 1e-6 && (via_map.out_rms - 0.25).abs() < 1e-6);
        assert!((via_map.sample_rate - 96000.0).abs() < 1e-3);
        assert_eq!(via_map.spectrum, spectrum);

        let via_fresh = read_telemetry_fresh(&path).expect("fresh read");
        assert!((via_fresh.out_peak - 0.6).abs() < 1e-6);
        assert!((via_fresh.sample_rate - 96000.0).abs() < 1e-3);
        assert_eq!(via_fresh.spectrum, spectrum);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn sharedfile_open_creates_then_preserves_header() {
        let path = temp_path("hdr");
        {
            let mut w = ApoStateWriter::create(&path).unwrap();
            w.publish(
                &ProcessorChain::builder()
                    .channels(2)
                    .sample_rate(48000.0)
                    .build(),
            );
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len() as usize, STATE_SIZE);
        // Re-open: header valid, generation NOT reset (preserved across opens).
        let r = SharedFile::open(&path).unwrap();
        assert!(r.generation() >= 2, "generation preserved on re-open");
        assert!(r.read().is_some());
        std::fs::remove_file(&path).ok();
    }
}
