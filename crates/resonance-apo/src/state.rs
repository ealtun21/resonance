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
use resonance_dsp::filter::{ApoFilter, BandScope, DynParams, FilterType};

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
/// v5: + convolution (enabled flag + IR-blob generation; samples live in the
///     sidecar blob file, see [`default_ir_path`]).
/// v6: + per-band dynamic EQ (enabled flag + threshold/range/attack/release).
/// v7: + linear-phase EQ mode flag.
/// v8: + transient per-band solo (audition one band; `SOLO_NONE` = off).
/// v9: + audition mode (solo/listen) beside the `solo_band` index.
/// v10: + daemon liveness heartbeat (APO auto-bypass when stale).
pub const STATE_VERSION: u32 = 10;

/// `solo_band` sentinel meaning "no band soloed" (the field is a fixed `u32`, so
/// `Option` is encoded as this reserved value rather than a niche).
pub const SOLO_NONE: u32 = u32::MAX;

/// `"RIRB"` little-endian — sanity tag for the IR-blob sidecar.
pub const IR_BLOB_MAGIC: u32 = 0x4252_4952;
/// IR-blob layout version.
pub const IR_BLOB_VERSION: u32 = 1;
/// Upper bound on stored IR samples (all channels summed) — bounds the blob
/// file and the APO-side read (2 s cap × 8 ch × 192 kHz ≈ 3 M samples).
pub const IR_BLOB_MAX_SAMPLES: usize = 4 << 20;

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
    /// Filter slope in dB/oct (12/24/48) for shelves + HP/LP; `0`/`12` = single
    /// biquad. Ignored by the single-biquad band types.
    pub slope_db_oct: u32,
    /// Stereo scope: `0` = Stereo (default), `1` = Mid, `2` = Side.
    pub scope: u32,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    /// `ChannelMask` bits — which channels this band applies to. `u64::MAX` (all
    /// bits) = global. Channel-count-independent, so it works on any APO format.
    pub channels: u64,
    /// `1` = dynamic EQ active on this band (Peaking only); the four params
    /// below are only meaningful when set.
    pub dyn_enabled: u32,
    _pad_dyn: u32,
    pub dyn_threshold_db: f64,
    pub dyn_range_db: f64,
    pub dyn_attack_ms: f64,
    pub dyn_release_ms: f64,
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
    pub crossfeed: EffectSnapshot,
    pub num_filters: u32,
    /// Output dither target bit depth (`0` = off; else 16/20/24).
    pub dither_bits: u32,
    /// Convolution stage: `0` = no IR; else the generation stamp of the IR-blob
    /// sidecar (see [`default_ir_path`]) whose samples to convolve. The APO
    /// reloads the blob when this changes.
    pub convolution_generation: u32,
    /// `1` = convolve, `0` = loaded-but-bypassed (or none).
    pub convolution_enabled: u32,
    /// `1` = linear-phase EQ (static bands rendered to a FIR by the APO's
    /// worker thread), `0` = minimum phase (biquads).
    pub phase_mode: u32,
    /// Transiently auditioned band index, or [`SOLO_NONE`] for no audition.
    /// While set the APO chain isolates that band (linear-phase suspended,
    /// matching the daemon).
    pub solo_band: u32,
    /// Audition mode for `solo_band`: 0 = Solo (bypass others), 1 = Listen
    /// (band-pass the band's region). Ignored when `solo_band == SOLO_NONE`.
    pub audition_mode: u32,
    _pad_audition: u32,
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
            crossfeed: EffectSnapshot::default(),
            num_filters: 0,
            dither_bits: 0,
            convolution_generation: 0,
            convolution_enabled: 0,
            phase_mode: 0,
            solo_band: SOLO_NONE,
            audition_mode: 0,
            _pad_audition: 0,
            filters: [FilterSnapshot::default(); MAX_FILTERS],
            route_channels: 0,
            _pad_route: 0,
            route_gains: [0.0; MAX_ROUTE * MAX_ROUTE],
        }
    }
}

/// Encode an audition band index for the fixed `solo_band` slot
/// ([`SOLO_NONE`] = no audition).
fn audition_index_encode(band: usize) -> u32 {
    u32::try_from(band)
        .ok()
        .filter(|&i| i != SOLO_NONE)
        .unwrap_or(SOLO_NONE)
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
    /// Daemon liveness stamp: a counter the daemon bumps ~every 30 ms while it
    /// runs. The APO worker bypasses the chain when it stops advancing (daemon
    /// quit, killed, or crashed) so EQ never outlives its control plane.
    pub heartbeat: AtomicU64,
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
        FxEffect::Crossfeed => (chain.crossfeed.intensity(), chain.crossfeed.enabled()),
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
            let dynamics = f.dynamics();
            let dp = dynamics.unwrap_or(DynParams::DEFAULT);
            *dst = FilterSnapshot {
                kind: filter_type_to_u32(f.filter_type),
                enabled: u32::from(f.enabled),
                slope_db_oct: u32::from(f.slope_db_oct),
                scope: scope_to_u32(f.scope),
                freq: f.freq,
                gain_db: f.gain_db,
                q: f.q,
                channels: f.mask.bits(),
                dyn_enabled: u32::from(dynamics.is_some()),
                _pad_dyn: 0,
                dyn_threshold_db: dp.threshold_db,
                dyn_range_db: dp.range_db,
                dyn_attack_ms: dp.attack_ms,
                dyn_release_ms: dp.release_ms,
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
            crossfeed: effect(chain, FxEffect::Crossfeed),
            num_filters: n as u32,
            dither_bits: chain.dither.bits().unwrap_or(0),
            // The blob generation is owned by the SharedFile writer (it knows
            // when the sidecar was written); `publish` stamps it after this.
            convolution_generation: 0,
            convolution_enabled: u32::from(chain.convolution.enabled()),
            phase_mode: u32::from(chain.phase_mode == resonance_dsp::chain::PhaseMode::Linear),
            solo_band: chain
                .audition
                .map_or(SOLO_NONE, |a| audition_index_encode(a.band)),
            audition_mode: chain.audition.map_or(0, |a| {
                u32::from(a.mode == resonance_dsp::chain::AuditionMode::Listen)
            }),
            _pad_audition: 0,
            filters,
            route_channels,
            _pad_route: 0,
            route_gains,
        }
    }

    /// Decode the transient audition ([`SOLO_NONE`] → `None`).
    #[must_use]
    pub fn audition(&self) -> Option<resonance_dsp::chain::BandAudition> {
        (self.solo_band != SOLO_NONE).then_some(resonance_dsp::chain::BandAudition {
            band: self.solo_band as usize,
            mode: if self.audition_mode == 1 {
                resonance_dsp::chain::AuditionMode::Listen
            } else {
                resonance_dsp::chain::AuditionMode::Solo
            },
        })
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
                .slope_db_oct(f.slope_db_oct as u8)
                .scope(scope_from_u32(f.scope))
                .dynamics(snapshot_dynamics(f))
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
        if self.phase_mode != 0 {
            // Arm the mode only — the FIR kernel is rendered off-RT by the
            // caller (worker / format lock), never here.
            chain.set_phase_mode(resonance_dsp::chain::PhaseMode::Linear);
        }
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
        chain.set_effect_intensity(FxEffect::Crossfeed, self.crossfeed.intensity);
        chain.set_effect_enabled(FxEffect::Crossfeed, self.crossfeed.enabled != 0);
        chain.set_dither((self.dither_bits != 0).then_some(self.dither_bits));
        // Transient audition: solo forces the IIR path in the chain, so it works
        // regardless of the phase mode armed above.
        chain.set_audition(self.audition());
        chain
    }

    /// Apply these parameters to an EXISTING chain in place, preserving filter
    /// and effect state so live edits (dragging EQ bands) don't reset biquad
    /// history → clicks. Returns `false` if the band structure changed (count
    /// differs) and the caller should rebuild instead.
    pub fn apply_to(&self, chain: &mut ProcessorChain, sample_rate: f64) -> bool {
        // Linear phase (either side of the change) always takes the rebuild
        // path: the FIR kernel must be re-rendered off-RT by the worker, and
        // that render needs the fresh band table a full rebuild provides.
        if self.phase_mode != 0 || chain.phase_mode == resonance_dsp::chain::PhaseMode::Linear {
            return false;
        }
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
        chain.set_effect_intensity(FxEffect::Crossfeed, self.crossfeed.intensity);
        chain.set_effect_enabled(FxEffect::Crossfeed, self.crossfeed.enabled != 0);
        chain.set_dither((self.dither_bits != 0).then_some(self.dither_bits));
        // Transient solo applies in place (a cheap flag; no filter-state reset,
        // so toggling an audition never clicks on the minimum-phase path).
        chain.set_audition(self.audition());

        // Routing is format-independent state on the chain; apply it in place at
        // the chain's live width (square-only; mismatched/absent → passthrough).
        chain.routing = route_matrix(self.route_channels, &self.route_gains, chain.channels);

        // Convolution: the bypass flag toggles in place. IR presence changing
        // (loaded↔none) needs the caller to reload the blob and rebuild; an IR
        // *content* change (same presence, new generation) is detected by the
        // worker's own generation tracking, not here.
        if (self.convolution_generation != 0) != chain.convolution.source().is_some() {
            return false;
        }
        chain
            .convolution
            .set_enabled(self.convolution_enabled != 0 && chain.convolution.source().is_some());

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
            let _ = slot.set_slope(f.slope_db_oct as u8, sample_rate);
            let _ = slot.set_dynamics(snapshot_dynamics(f), sample_rate);
            slot.scope = scope_from_u32(f.scope);
            slot.enabled = f.enabled != 0;
            // Per-channel target is plain state (no coefficient/history impact).
            slot.mask = ChannelMask::from_bits(f.channels);
        }
        true
    }
}

/// The dynamic-EQ params carried by a filter snapshot, `None` when unset.
#[must_use]
pub fn snapshot_dynamics(f: &FilterSnapshot) -> Option<DynParams> {
    (f.dyn_enabled != 0).then_some(DynParams {
        threshold_db: f.dyn_threshold_db,
        range_db: f.dyn_range_db,
        attack_ms: f.dyn_attack_ms,
        release_ms: f.dyn_release_ms,
    })
}

/// Stable `BandScope` → `u32` mapping for the shared block (must round-trip).
#[must_use]
pub fn scope_to_u32(s: BandScope) -> u32 {
    match s {
        BandScope::Stereo => 0,
        BandScope::Mid => 1,
        BandScope::Side => 2,
    }
}

/// Inverse of [`scope_to_u32`]; unknown values fall back to `Stereo`.
#[must_use]
pub fn scope_from_u32(v: u32) -> BandScope {
    match v {
        1 => BandScope::Mid,
        2 => BandScope::Side,
        _ => BandScope::Stereo,
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

/// IR-blob sidecar path for a given state file: `apo_state.bin` →
/// `apo_state.ir.blob` (same directory, so `audiodg.exe` can read it for the
/// same reason it can read the state file).
#[must_use]
pub fn ir_path_for(state_path: &Path) -> PathBuf {
    state_path.with_extension("ir.blob")
}

/// Default IR-blob sidecar path (pairs with [`default_state_path`]).
#[must_use]
pub fn default_ir_path() -> PathBuf {
    ir_path_for(&default_state_path())
}

/// Write the impulse response as a sidecar blob the APO can read: a fixed
/// little-endian header (`magic, version, generation, channels, frames, pad,
/// rate: f64`) followed by the samples channel-planar as `f32`. Written to a
/// sibling temp file then renamed, so the APO never observes a torn blob.
///
/// # Errors
///
/// Returns an [`io::Error`] when the file cannot be written or renamed, or the
/// IR exceeds [`IR_BLOB_MAX_SAMPLES`].
pub fn write_ir_blob(
    path: &Path,
    generation: u32,
    ir: &resonance_dsp::convolution::IrData,
) -> io::Result<()> {
    let channels = ir.channels.len();
    let frames = ir.frames();
    let total = channels.saturating_mul(frames);
    if total == 0 || total > IR_BLOB_MAX_SAMPLES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("IR blob size {total} samples out of range"),
        ));
    }
    let mut buf = Vec::with_capacity(32 + total * 4);
    buf.extend_from_slice(&IR_BLOB_MAGIC.to_le_bytes());
    buf.extend_from_slice(&IR_BLOB_VERSION.to_le_bytes());
    buf.extend_from_slice(&generation.to_le_bytes());
    buf.extend_from_slice(&(channels as u32).to_le_bytes());
    buf.extend_from_slice(&(frames as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&ir.sample_rate.to_le_bytes());
    for ch in &ir.channels {
        for &s in ch {
            buf.extend_from_slice(&(s as f32).to_le_bytes());
        }
    }
    let tmp = path.with_extension("blob.tmp");
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, path)
}

/// Read + validate an IR blob. Returns `(generation, ir)` or `None` on any
/// mismatch (missing file, bad magic/version, inconsistent sizes) — the APO
/// treats every failure as "no IR" rather than erroring inside audiodg.
#[must_use]
pub fn read_ir_blob(path: &Path) -> Option<(u32, resonance_dsp::convolution::IrData)> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 32 {
        return None;
    }
    let u32_at = |o: usize| -> Option<u32> {
        Some(u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?))
    };
    let magic = u32_at(0)?;
    let version = u32_at(4)?;
    let generation = u32_at(8)?;
    let channels = u32_at(12)? as usize;
    let frames = u32_at(16)? as usize;
    if magic != IR_BLOB_MAGIC || version != IR_BLOB_VERSION {
        return None;
    }
    let rate = f64::from_le_bytes(data.get(24..32)?.try_into().ok()?);
    let total = channels.checked_mul(frames)?;
    if channels == 0
        || frames == 0
        || total > IR_BLOB_MAX_SAMPLES
        || data.len() < 32 + total * 4
        || rate <= 0.0
        || !rate.is_finite()
    {
        return None;
    }
    let mut chans = Vec::with_capacity(channels);
    let mut off = 32;
    for _ in 0..channels {
        let mut v = Vec::with_capacity(frames);
        for _ in 0..frames {
            let s = f32::from_le_bytes(data.get(off..off + 4)?.try_into().ok()?);
            if !s.is_finite() {
                return None;
            }
            v.push(f64::from(s));
            off += 4;
        }
        chans.push(v);
    }
    Some((
        generation,
        resonance_dsp::convolution::IrData {
            name: "apo-ir".to_string(),
            path: path.to_string_lossy().into_owned(),
            sample_rate: rate,
            channels: chans,
        },
    ))
}

/// The shared file, mapped read-write by both the daemon and the APO.
///
/// Two independent seqlock regions: the daemon owns the chain `snapshot`
/// (writes / the APO reads), the APO owns `telemetry` (writes / the daemon
/// reads). The daemon owns the `telemetry_enabled` gate.
pub struct SharedFile {
    mmap: memmap2::MmapMut,
    /// Writer-side IR sidecar tracking (daemon only): the `Arc` identity of the
    /// last IR written to the blob, and the generation stamped on it. `0` ptr =
    /// none written this session.
    last_ir_ptr: usize,
    ir_generation: u32,
    ir_path: PathBuf,
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
        // Seed the IR generation from wall time so a daemon restart never
        // reuses a previous session's stamp with different blob contents.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| (d.as_secs() as u32) | 1);
        let this = Self {
            mmap,
            last_ir_ptr: 0,
            ir_generation: seed,
            ir_path: ir_path_for(path),
        };
        // SAFETY: map is STATE_SIZE bytes; SharedState is repr(C).
        let st = unsafe { &*this.mmap.as_ptr().cast::<SharedState>() };
        if st.magic != STATE_MAGIC || st.version != STATE_VERSION {
            let st = unsafe { &mut *(this.mmap.as_ptr() as *mut SharedState) };
            st.magic = STATE_MAGIC;
            st.version = STATE_VERSION;
            st.generation.store(0, Ordering::Release);
            st.telemetry_enabled.store(0, Ordering::Release);
            st.telemetry.generation.store(0, Ordering::Release);
            st.heartbeat.store(0, Ordering::Release);
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

    /// Publish the daemon's current chain under the seqlock, syncing the IR
    /// sidecar blob first so the APO never sees a generation whose samples
    /// aren't on disk yet.
    pub fn publish(&mut self, chain: &ProcessorChain) {
        let conv_gen = self.sync_ir_blob(chain);
        let mut snap = ChainSnapshot::from_chain(chain);
        snap.convolution_generation = conv_gen;
        let st = self.state_mut();
        let g = st.generation.load(Ordering::Relaxed);
        st.generation.store(g.wrapping_add(1), Ordering::Release); // odd: writing
        st.snapshot = snap;
        st.generation.store(g.wrapping_add(2), Ordering::Release); // even: done
    }

    /// Bump the daemon-liveness stamp (called ~every 30 ms by the daemon's
    /// telemetry pump). Not a seqlock write: a torn read of a monotonically
    /// increasing u64 still reads as "changed", which is all the APO needs.
    pub fn beat(&mut self) {
        let st = self.state_mut();
        let h = st.heartbeat.load(Ordering::Relaxed);
        st.heartbeat.store(h.wrapping_add(1), Ordering::Release);
    }

    /// Publish a bypass: keep the last chain parameters but force
    /// `enabled = 0`, so the APO passes audio through untouched. Called on
    /// graceful daemon shutdown — EQ must not outlive the control plane.
    pub fn publish_bypass(&mut self) {
        let st = self.state_mut();
        let g = st.generation.load(Ordering::Relaxed);
        st.generation.store(g.wrapping_add(1), Ordering::Release); // odd: writing
        st.snapshot.enabled = 0;
        st.generation.store(g.wrapping_add(2), Ordering::Release); // even: done
    }

    /// Write the chain's IR to the sidecar blob when it changed (tracked by the
    /// source `Arc`'s identity). Returns the generation to stamp into the
    /// snapshot: `0` = no IR (or the blob write failed — the APO then treats it
    /// as no IR rather than convolving stale samples).
    fn sync_ir_blob(&mut self, chain: &ProcessorChain) -> u32 {
        let Some(ir) = chain.convolution.source() else {
            self.last_ir_ptr = 0;
            return 0;
        };
        let ptr = std::sync::Arc::as_ptr(ir) as usize;
        if ptr == self.last_ir_ptr {
            return self.ir_generation;
        }
        let next = self.ir_generation.wrapping_add(1).max(1);
        if write_ir_blob(&self.ir_path, next, ir).is_ok() {
            self.ir_generation = next;
            self.last_ir_ptr = ptr;
            self.ir_generation
        } else {
            self.last_ir_ptr = 0;
            0
        }
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

/// Read the daemon-liveness heartbeat with a fresh file read (a long-lived
/// mapped view does not observe the daemon's writes across sessions on
/// Windows — see `read_chain_fresh`). `None` = file missing/invalid, which
/// callers must treat as "daemon gone".
#[must_use]
pub fn read_heartbeat_fresh(path: &Path) -> Option<u64> {
    let b = std::fs::read(path).ok()?;
    if b.len() < STATE_SIZE
        || read_u32(&b, 0)? != STATE_MAGIC
        || read_u32(&b, std::mem::offset_of!(SharedState, version))? != STATE_VERSION
    {
        return None;
    }
    read_u64(&b, std::mem::offset_of!(SharedState, heartbeat))
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

    fn test_ir(taps: Vec<f64>) -> std::sync::Arc<resonance_dsp::convolution::IrData> {
        std::sync::Arc::new(resonance_dsp::convolution::IrData {
            name: "t".into(),
            path: "/t.wav".into(),
            sample_rate: 48_000.0,
            channels: vec![taps],
        })
    }

    #[test]
    fn ir_blob_round_trips_and_rejects_corruption() {
        let path = temp_path("irblob").with_extension("blob");
        let ir = resonance_dsp::convolution::IrData {
            name: "x".into(),
            path: "/x.wav".into(),
            sample_rate: 44_100.0,
            channels: vec![vec![1.0, 0.5, -0.25], vec![0.0, 0.125, 0.75]],
        };
        write_ir_blob(&path, 7, &ir).unwrap();
        let (generation, back) = read_ir_blob(&path).expect("blob reads back");
        assert_eq!(generation, 7);
        assert_eq!(back.channels.len(), 2);
        assert_eq!(back.frames(), 3);
        assert_eq!(back.sample_rate, 44_100.0);
        assert!((back.channels[0][1] - 0.5).abs() < 1e-6);
        assert!((back.channels[1][2] - 0.75).abs() < 1e-6);

        // Truncated + garbage files must read as "no IR", never panic.
        let data = std::fs::read(&path).unwrap();
        std::fs::write(&path, &data[..20]).unwrap();
        assert!(read_ir_blob(&path).is_none(), "truncated header rejected");
        std::fs::write(&path, &data[..data.len() - 4]).unwrap();
        assert!(read_ir_blob(&path).is_none(), "truncated samples rejected");
        std::fs::write(&path, b"not a blob at all").unwrap();
        assert!(read_ir_blob(&path).is_none(), "garbage rejected");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn publish_stamps_ir_generation_and_reuses_it_for_same_arc() {
        let path = temp_path("irgen");
        let mut w = ApoStateWriter::create(&path).unwrap();
        let r = SharedFile::open(&path).unwrap();
        let blob = ir_path_for(&path);

        // No IR → generation 0, enabled 0.
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .build();
        w.publish(&chain);
        let snap = r.read().unwrap();
        assert_eq!(snap.convolution_generation, 0);
        assert_eq!(snap.convolution_enabled, 0);

        // IR loaded → non-zero generation, blob on disk with the same stamp.
        chain.convolution.load_ir(test_ir(vec![1.0, 0.5])).unwrap();
        w.publish(&chain);
        let snap1 = r.read().unwrap();
        assert_ne!(snap1.convolution_generation, 0);
        assert_eq!(snap1.convolution_enabled, 1);
        let (blob_gen, blob_ir) = read_ir_blob(&blob).expect("sidecar written");
        assert_eq!(blob_gen, snap1.convolution_generation);
        assert_eq!(blob_ir.frames(), 2);

        // Same Arc republished (e.g. bypass toggle) → SAME generation, so the
        // APO doesn't re-prepare the kernel.
        chain.convolution.set_enabled(false);
        w.publish(&chain);
        let snap2 = r.read().unwrap();
        assert_eq!(snap2.convolution_generation, snap1.convolution_generation);
        assert_eq!(snap2.convolution_enabled, 0);

        // A NEW IR bumps the generation.
        chain.convolution.load_ir(test_ir(vec![0.25])).unwrap();
        w.publish(&chain);
        let snap3 = r.read().unwrap();
        assert_ne!(snap3.convolution_generation, snap1.convolution_generation);
        let (g3, ir3) = read_ir_blob(&blob).unwrap();
        assert_eq!(g3, snap3.convolution_generation);
        assert_eq!(ir3.frames(), 1);

        // Clearing goes back to generation 0.
        chain.convolution.clear();
        w.publish(&chain);
        assert_eq!(r.read().unwrap().convolution_generation, 0);

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&blob).ok();
    }

    #[test]
    fn apply_to_handles_convolution_presence_and_bypass() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .build();
        let mut snap = ChainSnapshot::from_chain(&chain);

        // Presence mismatch (snapshot has an IR, chain doesn't) → rebuild.
        snap.convolution_generation = 5;
        snap.convolution_enabled = 1;
        assert!(
            !snap.apply_to(&mut chain, 48_000.0),
            "IR appearing must force a rebuild"
        );

        // With the IR loaded, the bypass flag toggles in place.
        chain.convolution.load_ir(test_ir(vec![1.0])).unwrap();
        assert!(snap.apply_to(&mut chain, 48_000.0));
        assert!(chain.convolution.enabled());
        snap.convolution_enabled = 0;
        assert!(snap.apply_to(&mut chain, 48_000.0));
        assert!(!chain.convolution.enabled(), "bypass applied in place");

        // IR disappearing from the snapshot → rebuild.
        snap.convolution_generation = 0;
        assert!(
            !snap.apply_to(&mut chain, 48_000.0),
            "IR removal must force a rebuild"
        );
    }

    #[test]
    fn audition_round_trips_and_applies_in_place() {
        use resonance_dsp::chain::{AuditionMode, BandAudition};
        use resonance_dsp::filter::{ApoFilter, FilterType};
        let mk_band = |f: f64| {
            ApoFilter::builder()
                .filter_type(FilterType::Peaking)
                .freq(f)
                .gain_db(6.0)
                .q(1.0)
                .channels(2)
                .sample_rate(48_000.0)
                .build()
                .unwrap()
        };
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(mk_band(200.0))
            .add_filter(mk_band(5_000.0))
            .build();

        // No audition by default; the sentinel round-trips to None.
        let snap = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap.solo_band, SOLO_NONE);
        assert_eq!(snap.audition(), None);

        // Listen band 1 → carried through the snapshot (mode=1) and applied.
        chain.set_audition(Some(BandAudition {
            band: 1,
            mode: AuditionMode::Listen,
        }));
        let snap = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap.audition_mode, 1);
        assert_eq!(
            snap.audition(),
            Some(BandAudition {
                band: 1,
                mode: AuditionMode::Listen
            })
        );
        let mut fresh = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(mk_band(200.0))
            .add_filter(mk_band(5_000.0))
            .build();
        assert!(
            snap.apply_to(&mut fresh, 48_000.0),
            "audition applies in place"
        );
        assert_eq!(
            fresh.audition,
            Some(BandAudition {
                band: 1,
                mode: AuditionMode::Listen
            })
        );

        // A rebuild (build_chain) also honours the audition.
        let rebuilt = snap.build_chain(2, 48_000.0);
        assert_eq!(
            rebuilt.audition,
            Some(BandAudition {
                band: 1,
                mode: AuditionMode::Listen
            })
        );

        // A Solo audition encodes mode=0.
        chain.set_audition(Some(BandAudition {
            band: 0,
            mode: AuditionMode::Solo,
        }));
        let snap = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap.audition_mode, 0);
        assert_eq!(
            snap.audition(),
            Some(BandAudition {
                band: 0,
                mode: AuditionMode::Solo
            })
        );

        // Clearing round-trips back to None in place.
        chain.set_audition(None);
        let snap = ChainSnapshot::from_chain(&chain);
        assert!(snap.apply_to(&mut fresh, 48_000.0));
        assert_eq!(fresh.audition, None);
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
    fn phase_mode_round_trips_and_forces_rebuild() {
        use resonance_dsp::chain::PhaseMode;
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(1000.0)
                    .gain_db(6.0)
                    .q(1.0)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48000.0)
                    .build()
                    .unwrap(),
            )
            .build();
        chain.set_phase_mode(PhaseMode::Linear);

        let snap = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap.phase_mode, 1);
        let rebuilt = snap.build_chain(2, 48000.0);
        assert_eq!(rebuilt.phase_mode, PhaseMode::Linear);

        // While linear, every publish takes the rebuild path so the worker
        // re-renders the kernel off-RT — apply_to must decline.
        let mut live = rebuilt;
        assert!(!snap.apply_to(&mut live, 48000.0));

        // Minimum-phase snapshots keep the click-free in-place path.
        chain.set_phase_mode(PhaseMode::Minimum);
        let snap_min = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap_min.phase_mode, 0);
        let mut live_min = snap_min.build_chain(2, 48000.0);
        assert!(snap_min.apply_to(&mut live_min, 48000.0));
    }

    #[test]
    fn band_dynamics_round_trip_through_snapshot() {
        use resonance_dsp::filter::DynParams;

        let dp = DynParams {
            threshold_db: -35.0,
            range_db: -9.0,
            attack_ms: 3.0,
            release_ms: 200.0,
        };
        let band = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(6000.0)
            .gain_db(0.0)
            .q(3.0)
            .dynamics(Some(dp))
            .enabled(true)
            .channels(2)
            .sample_rate(48000.0)
            .build()
            .unwrap();
        let chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(band)
            .build();

        let snap = ChainSnapshot::from_chain(&chain);
        assert_eq!(snap.filters[0].dyn_enabled, 1);
        assert!((snap.filters[0].dyn_threshold_db - (-35.0)).abs() < 1e-9);

        // Full rebuild carries the dynamics.
        let rebuilt = snap.build_chain(2, 48000.0);
        assert_eq!(rebuilt.filters[0].dynamics(), Some(dp));

        // The in-place apply path attaches/updates them too.
        let mut live = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(6000.0)
                    .gain_db(0.0)
                    .q(3.0)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48000.0)
                    .build()
                    .unwrap(),
            )
            .build();
        assert!(snap.apply_to(&mut live, 48000.0));
        assert_eq!(live.filters[0].dynamics(), Some(dp));

        // And a snapshot without dynamics clears them in place.
        let snap_off = ChainSnapshot::from_chain(&live_chain_without_dynamics());
        assert!(snap_off.apply_to(&mut live, 48000.0));
        assert!(live.filters[0].dynamics().is_none());
    }

    fn live_chain_without_dynamics() -> ProcessorChain {
        ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(6000.0)
                    .gain_db(0.0)
                    .q(3.0)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48000.0)
                    .build()
                    .unwrap(),
            )
            .build()
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

    #[test]
    fn heartbeat_beats_and_reads_fresh() {
        let path = temp_path("hb");
        let mut w = ApoStateWriter::create(&path).unwrap();
        let h0 = read_heartbeat_fresh(&path).unwrap();
        w.beat();
        w.beat();
        assert_eq!(read_heartbeat_fresh(&path).unwrap(), h0 + 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn publish_bypass_zeroes_enabled_and_advances_generation() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .preamp_db(-3.5)
            .build();
        chain.enabled = true;
        let path = temp_path("byp");
        let mut w = ApoStateWriter::create(&path).unwrap();
        w.publish(&chain);
        let (g1, s1, _) = read_chain_fresh(&path).unwrap();
        assert_eq!(s1.enabled, 1);

        w.publish_bypass();
        let (g2, s2, _) = read_chain_fresh(&path).unwrap();
        assert_eq!(s2.enabled, 0, "bypass must publish enabled=0");
        assert!(g2 > g1, "generation must advance so the worker notices");
        assert!(
            (s2.preamp_db - (-3.5)).abs() < 1e-12,
            "other params preserved"
        );
        std::fs::remove_file(&path).ok();
    }
}
