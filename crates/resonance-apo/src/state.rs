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
use resonance_dsp::effects::Effect;
use resonance_dsp::filter::{ApoFilter, FilterType};

/// Maximum number of EQ bands carried in the shared block.
pub const MAX_FILTERS: usize = 32;
/// `"RAPO"` little-endian — sanity tag for the shared block.
pub const STATE_MAGIC: u32 = 0x4F50_4152;
/// Layout version; bump on any `#[repr(C)]` change below.
pub const STATE_VERSION: u32 = 2;

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
    pub num_filters: u32,
    pub filters: [FilterSnapshot; MAX_FILTERS],
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
            num_filters: 0,
            filters: [FilterSnapshot::default(); MAX_FILTERS],
        }
    }
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
    pub _pad: f32,
    pub spectrum: [f32; TELEMETRY_BINS],
}

/// Plain copy of telemetry returned to readers.
#[derive(Clone, Copy)]
pub struct TelemetrySnapshot {
    pub in_peak: f32,
    pub out_peak: f32,
    pub in_rms: f32,
    pub out_rms: f32,
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
    pub _pad2: u32,
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
    };
    EffectSnapshot {
        enabled: enabled as u32,
        intensity,
    }
}

impl ChainSnapshot {
    /// Capture the daemon's current chain (format-independent parameters only).
    pub fn from_chain(chain: &ProcessorChain) -> Self {
        let mut filters = [FilterSnapshot::default(); MAX_FILTERS];
        let n = chain.filters.len().min(MAX_FILTERS);
        for (dst, f) in filters.iter_mut().zip(chain.filters.iter()).take(n) {
            *dst = FilterSnapshot {
                kind: filter_type_to_u32(f.filter_type),
                enabled: f.enabled as u32,
                freq: f.freq,
                gain_db: f.gain_db,
                q: f.q,
            };
        }
        Self {
            enabled: chain.enabled as u32,
            preamp_db: chain.preamp_db,
            fidelity: effect(chain, FxEffect::Fidelity),
            ambience: effect(chain, FxEffect::Ambience),
            surround: effect(chain, FxEffect::Surround),
            dynamic_boost: effect(chain, FxEffect::DynamicBoost),
            bass: effect(chain, FxEffect::Bass),
            num_filters: n as u32,
            filters,
        }
    }

    /// Rebuild a `ProcessorChain` at the APO's negotiated format, applying these
    /// parameters. Filters that fail to build (bad coeffs) are skipped.
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
                .build()
            {
                builder = builder.add_filter(filter);
            }
        }

        let mut chain = builder.build();
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
        }
        true
    }
}

/// Stable `FilterType` → `u32` mapping for the shared block (must round-trip).
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
pub fn default_state_path() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
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
        let st = unsafe { &*(this.mmap.as_ptr() as *const SharedState) };
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
    pub fn create(path: &Path) -> io::Result<Self> {
        Self::open(path)
    }

    fn state(&self) -> &SharedState {
        // SAFETY: map is STATE_SIZE bytes; SharedState is repr(C).
        unsafe { &*(self.mmap.as_ptr() as *const SharedState) }
    }

    #[allow(clippy::mut_from_ref)]
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
    pub fn generation(&self) -> u64 {
        self.state().generation.load(Ordering::Acquire)
    }

    /// Read a consistent chain snapshot (seqlock retry), or `None`.
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
            .store(on as u32, Ordering::Release);
    }

    /// APO: is a client watching? Cheap atomic load on the audio thread.
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
        spectrum: &[f32; TELEMETRY_BINS],
    ) {
        let t = &mut self.state_mut().telemetry;
        let g = t.generation.load(Ordering::Relaxed);
        t.generation.store(g.wrapping_add(1), Ordering::Release); // odd
        t.in_peak = in_peak;
        t.out_peak = out_peak;
        t.in_rms = in_rms;
        t.out_rms = out_rms;
        t.spectrum = *spectrum;
        t.generation.store(g.wrapping_add(2), Ordering::Release); // even
    }

    /// Daemon: read a consistent telemetry snapshot (seqlock retry), or `None`.
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

#[cfg(test)]
mod tests {
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
}
