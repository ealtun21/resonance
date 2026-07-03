use crate::channel::ChannelMatrix;
use crate::convolution::ConvolutionEngine;
use crate::dither::DitherStage;
use crate::effects::{
    AmbienceEffect, BassBoostEffect, CrossfeedEffect, DynamicBoostEffect, Effect, FidelityEffect,
    LoudnessEffect, SurroundEffect,
};
use crate::filter::{ApoFilter, BandScope};

/// EQ phase behaviour: `Minimum` = the biquad bank (today's path, zero
/// latency), `Linear` = the static stereo bands are rendered to a symmetric
/// FIR (see [`crate::linphase`]) — no phase rotation, `BLOCK + N/2` samples of
/// added latency. Mid/Side and dynamic bands stay on the IIR path either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhaseMode {
    #[default]
    Minimum,
    Linear,
}

/// Per-band audition mode. `Solo` bypasses every other band (hear this band's
/// effect on the full signal); `Listen` bypasses ALL bands and runs one
/// band-pass/low-pass/high-pass isolating this band's operating region at unity
/// gain (hear the raw content there, regardless of the band's boost/cut).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditionMode {
    Solo,
    Listen,
}

/// A transient single-band audition: which band, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandAudition {
    pub band: usize,
    pub mode: AuditionMode,
}

/// Output-envelope state for click-free FIR-path transitions: the audible
/// switch (mode flip or kernel swap) is deferred until a short fade-out
/// completes, then the new path fades back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FirFade {
    #[default]
    Stable,
    Out,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FxEffect {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
    Loudness,
    Crossfeed,
}

impl FxEffect {
    /// Every effect, in chain order. Adding a variant forces this array to be
    /// updated, propagating to every `ALL` iteration.
    pub const ALL: [FxEffect; 7] = [
        FxEffect::Fidelity,
        FxEffect::Ambience,
        FxEffect::Surround,
        FxEffect::DynamicBoost,
        FxEffect::Bass,
        FxEffect::Loudness,
        FxEffect::Crossfeed,
    ];
}

#[derive(Debug, Clone)]
pub struct ProcessorChain {
    pub channels: usize,
    pub sample_rate: f64,
    pub enabled: bool,
    pub preamp_db: f64,
    pub filters: Vec<ApoFilter>,
    /// Impulse-response convolution (room/speaker correction, HRTF). Runs right
    /// after the filter bank — the two linear stages sit together, ahead of the
    /// nonlinear effects. Off + empty by default (bit-exact passthrough).
    pub convolution: ConvolutionEngine,
    /// EQ phase behaviour; `Minimum` (default) leaves this chain byte-identical
    /// to the pre-linear-phase build.
    pub phase_mode: PhaseMode,
    /// The linear-phase FIR realisation of the static filter bank. Loaded by
    /// the daemon/APO (never rendered on the RT thread); only consulted while
    /// [`ProcessorChain::phase_mode`] is `Linear` AND a kernel is present —
    /// otherwise the IIR bank keeps running, so a pending or failed render can
    /// never leave a flat/silent gap.
    pub eq_fir: ConvolutionEngine,
    /// Whether the FIR path is the one currently audible. Lags the *desired*
    /// state (mode + kernel) by one short fade so switches are click-free.
    fir_active: bool,
    /// Identity of the kernel `fir_active` was faded in with (Arc pointer):
    /// a swap while active (band edit re-render) also rides the fade.
    fir_kernel_id: usize,
    fir_fade: FirFade,
    /// Output gain during a transition (1 = untouched; Stable skips the
    /// multiply entirely so the steady path stays bit-exact).
    fir_gain: f64,
    /// Input ramp into the FIR after a flip: the engine starts from reset, so
    /// without this the mid-stream input onset is a hard cut whose (faithful)
    /// convolution arrives `latency` later as a click — long after the output
    /// fade has ended. Ramping the input makes the delayed arrival smooth.
    fir_in_gain: f64,
    pub fidelity: FidelityEffect,
    pub ambience: AmbienceEffect,
    pub surround: SurroundEffect,
    pub dynamic_boost: DynamicBoostEffect,
    pub bass: BassBoostEffect,
    pub loudness: LoudnessEffect,
    pub crossfeed: CrossfeedEffect,
    /// Final-stage TPDF dither (off by default → bit-exact).
    pub dither: DitherStage,
    /// Optional output remap applied *after* EQ + effects, mapping the `channels`
    /// processed channels to a (possibly different) output channel count: swap,
    /// permutation, duplication, drop, up/downmix. `None` (or a square identity)
    /// is a zero-cost passthrough — see [`ProcessorChain::route`].
    pub routing: Option<ChannelMatrix>,
    /// Transient per-band audition (Solo or Listen). Runtime-only: not part of
    /// the builder, never persisted to a `Profile`, cleared on release, and
    /// auto-cleared by the daemon on any band-table edit. Forces the IIR path
    /// (suspends linear-phase for the duration) so the cascade skip isolates the
    /// band directly. Effects/convolution/crossfeed/dither still run — the
    /// audition isolates only among the EQ bands.
    pub audition: Option<BandAudition>,
    /// The prepared Listen filter (band-pass/low-pass/high-pass at the target
    /// band's Fc/Q). Built off the RT hot path by [`ProcessorChain::set_audition`]
    /// when the mode is `Listen`; `None` for Solo / no audition.
    audition_filter: Option<ApoFilter>,
}

impl ProcessorChain {
    #[must_use]
    pub fn builder() -> ProcessorChainBuilder {
        ProcessorChainBuilder::default()
    }

    /// Process an interleaved buffer of f64 samples in place.
    pub fn process(&mut self, buf: &mut [f64]) {
        if !self.enabled || buf.is_empty() {
            return;
        }

        let channels = self.channels;

        if self.preamp_db != 0.0 {
            let gain = db_to_linear(self.preamp_db);
            for s in buf.iter_mut() {
                *s *= gain;
            }
        }

        // Linear phase engages only with a loaded kernel (see `eq_fir` docs).
        // An active audition (Solo or Listen) forces the IIR path so the cascade
        // skip below isolates the band directly — linear-phase is suspended for
        // the audition's duration and fades back in (kernel retained) on clear.
        let fir_want = self.phase_mode == PhaseMode::Linear
            && self.audition.is_none()
            && self.eq_fir.enabled()
            && self.eq_fir.source().is_some();
        let kernel_id = self
            .eq_fir
            .source()
            .map_or(0, |a| std::sync::Arc::as_ptr(a).cast::<u8>() as usize);
        // The audible switch is deferred: fade out on the old path, flip at
        // silence, fade the new path in (see `FirFade`).
        match self.fir_fade {
            FirFade::Stable => {
                let swap = self.fir_active && fir_want && kernel_id != self.fir_kernel_id;
                if fir_want != self.fir_active || swap {
                    self.fir_fade = FirFade::Out;
                } else {
                    // Track silent kernel changes while inactive (no fade).
                    self.fir_kernel_id = kernel_id;
                }
            }
            FirFade::Out if self.fir_gain <= 0.0 => {
                self.fir_active = fir_want;
                self.fir_kernel_id = kernel_id;
                if fir_want {
                    // Fresh audible start for the (possibly swapped) kernel,
                    // with a ramped input onset (see `fir_in_gain`).
                    self.eq_fir.reset();
                    self.fir_in_gain = 0.0;
                } else if self.phase_mode != PhaseMode::Linear {
                    // Mode left Linear: drop the kernel now that it's silent.
                    self.eq_fir.clear();
                }
                self.fir_fade = FirFade::In;
            }
            _ => {}
        }
        let fir_active = self.fir_active;

        // Band-major cascade: each biquad makes one pass over the buffer. The
        // buffer is small enough to stay cache-resident across passes, so this
        // keeps a single filter's coefficients+state hot per pass — measurably
        // faster than a sample-major inner loop that cycles every band's state
        // on each sample once the band count grows.
        let audition = self.audition;
        for (idx, filter) in self.filters.iter_mut().enumerate() {
            // Transient audition: Solo runs only the target band; Listen skips
            // ALL bands (the audition filter below replaces them). Either way the
            // audition forces the IIR path (fir_active is false).
            if let Some(a) = audition {
                match a.mode {
                    AuditionMode::Solo if idx != a.band => continue,
                    AuditionMode::Solo => {}
                    AuditionMode::Listen => continue,
                }
            }
            // Bands realised by the FIR kernel skip the IIR pass; Mid/Side and
            // dynamic bands are not linearizable and stay here (hybrid mode).
            if fir_active && crate::linphase::is_linearizable(filter) {
                continue;
            }
            let frames = buf.len() / channels;
            match filter.scope {
                BandScope::Stereo if filter.dynamics_active() => {
                    // Dynamic band: frame-major so the linked detector (mean of
                    // the masked channels) morphs the gain before the frame's
                    // channels are filtered.
                    for frame in 0..frames {
                        let base = frame * channels;
                        let (mut sum, mut n) = (0.0, 0u32);
                        for ch in 0..channels {
                            if filter.mask.contains(ch) {
                                sum += buf[base + ch];
                                n += 1;
                            }
                        }
                        if n > 0 {
                            filter.dyn_detect(sum / f64::from(n));
                        }
                        for ch in 0..channels {
                            if filter.mask.contains(ch) {
                                buf[base + ch] = filter.process_channel(buf[base + ch], ch);
                            }
                        }
                    }
                }
                BandScope::Stereo => {
                    for frame in 0..frames {
                        for ch in 0..channels {
                            // Per-channel EQ: a band only touches the channels its
                            // mask selects; excluded channels pass through and
                            // their biquad state stays at rest. `ChannelMask::ALL`
                            // (the default) makes this a no-op branch — the global
                            // case.
                            if filter.mask.contains(ch) {
                                let idx = frame * channels + ch;
                                buf[idx] = filter.process_channel(buf[idx], ch);
                            }
                        }
                    }
                }
                // Mid/side: process the mono sum / stereo difference of the front
                // L/R pair (channels 0 and 1). Channels ≥2 pass through; the band
                // mask is not used (scope targets the front pair by definition).
                BandScope::Mid | BandScope::Side => {
                    if channels < 2 {
                        // Mono has no side information: Mid processes the single
                        // channel; Side is a no-op.
                        if filter.scope == BandScope::Mid {
                            for frame in 0..frames {
                                let idx = frame * channels;
                                buf[idx] = filter.dyn_process(buf[idx], 0);
                            }
                        }
                        continue;
                    }
                    for frame in 0..frames {
                        let il = frame * channels;
                        let ir = il + 1;
                        let (l, r) = (buf[il], buf[ir]);
                        let m = (l + r) * 0.5;
                        let s = (l - r) * 0.5;
                        // Mid uses biquad state slot 0, Side slot 1, so the two
                        // scopes never share running history within a band. The
                        // dynamics detector (if any) feeds off the same M/S
                        // value the band filters.
                        let (m2, s2) = if filter.scope == BandScope::Mid {
                            (filter.dyn_process(m, 0), s)
                        } else {
                            (m, filter.dyn_process(s, 1))
                        };
                        buf[il] = m2 + s2;
                        buf[ir] = m2 - s2;
                    }
                }
            }
        }

        // Listen mode: the isolated region is auditioned by one filter in place
        // of the (skipped) bands.
        if matches!(
            self.audition,
            Some(BandAudition {
                mode: AuditionMode::Listen,
                ..
            })
        ) {
            if let Some(f) = self.audition_filter.as_mut() {
                let frames = buf.len() / channels;
                for frame in 0..frames {
                    for ch in 0..channels {
                        let i = frame * channels + ch;
                        buf[i] = f.process_channel(buf[i], ch);
                    }
                }
            }
        }

        if fir_active {
            if self.fir_in_gain < 1.0 {
                let step = 1.0 / (self.sample_rate * 0.008).max(1.0);
                let frames = buf.len() / channels;
                for frame in 0..frames {
                    self.fir_in_gain = (self.fir_in_gain + step).min(1.0);
                    for s in &mut buf[frame * channels..(frame + 1) * channels] {
                        *s *= self.fir_in_gain;
                    }
                }
            }
            self.eq_fir.process(buf, channels);
        }
        // Transition envelope (8 ms raised ramp each way). Stable = no touch,
        // keeping the steady path bit-exact.
        if self.fir_fade != FirFade::Stable {
            let step = 1.0 / (self.sample_rate * 0.008).max(1.0);
            let frames = buf.len() / channels;
            for frame in 0..frames {
                match self.fir_fade {
                    FirFade::Out => self.fir_gain = (self.fir_gain - step).max(0.0),
                    FirFade::In => {
                        self.fir_gain = (self.fir_gain + step).min(1.0);
                        if self.fir_gain >= 1.0 {
                            self.fir_fade = FirFade::Stable;
                        }
                    }
                    FirFade::Stable => {}
                }
                for s in &mut buf[frame * channels..(frame + 1) * channels] {
                    *s *= self.fir_gain;
                }
            }
        }
        self.convolution.process(buf, channels);
        self.fidelity.process(buf, channels);
        self.ambience.process(buf, channels);
        self.surround.process(buf, channels);
        self.dynamic_boost.process(buf, channels);
        self.bass.process(buf, channels);
        self.loudness.process(buf, channels);
        // Crossfeed narrows the final stereo image, so it runs last — after every
        // other effect (including Surround, which widens it) has shaped the sound.
        self.crossfeed.process(buf, channels);
        // Dither is the very last stage, right before the output truncation.
        self.dither.apply(buf, channels);
    }

    /// Set (or clear) the output dither target bit depth. `None` = off.
    pub fn set_dither(&mut self, bits: Option<u32>) {
        self.dither.set_bits(bits);
    }

    /// Set (or clear) the transient per-band audition. For `Listen`, builds the
    /// type-aware audition filter from the target band; for `Solo`/`None` clears
    /// it. Out-of-range indices are accepted verbatim (they mute every band until
    /// cleared); callers validate against the live band count. Transient — never
    /// persisted.
    pub fn set_audition(&mut self, audition: Option<BandAudition>) {
        self.audition_filter = match audition {
            Some(BandAudition {
                band,
                mode: AuditionMode::Listen,
            }) => self
                .filters
                .get(band)
                .and_then(|b| build_audition_filter(b, self.channels, self.sample_rate)),
            _ => None,
        };
        self.audition = audition;
    }

    /// Switch the EQ phase behaviour. The audible change rides a short fade
    /// inside [`ProcessorChain::process`]; leaving `Linear` drops the FIR
    /// kernel once it has faded out (re-entering requires a fresh render —
    /// the daemon/APO owns that).
    pub fn set_phase_mode(&mut self, mode: PhaseMode) {
        self.phase_mode = mode;
    }

    /// Added latency of the linear-phase realisation in frames: the engine's
    /// fixed `BLOCK` plus the kernel's `N/2` group delay. Zero when the mode
    /// is off or no kernel is loaded.
    #[must_use]
    pub fn eq_fir_latency_frames(&self) -> usize {
        if self.phase_mode != PhaseMode::Linear {
            return 0;
        }
        self.eq_fir.source().map_or(0, |ir| {
            self.eq_fir.latency_frames() + ir.channels.first().map_or(0, |h| h.len() / 2)
        })
    }

    pub fn set_effect_intensity(&mut self, effect: FxEffect, value: f64) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_intensity(value),
            FxEffect::Ambience => self.ambience.set_intensity(value),
            FxEffect::Surround => self.surround.set_intensity(value),
            FxEffect::DynamicBoost => self.dynamic_boost.set_intensity(value),
            FxEffect::Bass => self.bass.set_intensity(value),
            FxEffect::Loudness => self.loudness.set_intensity(value),
            FxEffect::Crossfeed => self.crossfeed.set_intensity(value),
        }
    }

    /// `(intensity, enabled)` for one effect — the read counterpart of
    /// `set_effect_intensity` / `set_effect_enabled`, so callers can iterate
    /// `FxEffect::ALL` instead of unrolling all five effects by hand.
    #[must_use]
    pub fn effect_params(&self, effect: FxEffect) -> (f64, bool) {
        match effect {
            FxEffect::Fidelity => (self.fidelity.intensity(), self.fidelity.enabled()),
            FxEffect::Ambience => (self.ambience.intensity(), self.ambience.enabled()),
            FxEffect::Surround => (self.surround.intensity(), self.surround.enabled()),
            FxEffect::DynamicBoost => {
                (self.dynamic_boost.intensity(), self.dynamic_boost.enabled())
            }
            FxEffect::Bass => (self.bass.intensity(), self.bass.enabled()),
            FxEffect::Loudness => (self.loudness.intensity(), self.loudness.enabled()),
            FxEffect::Crossfeed => (self.crossfeed.intensity(), self.crossfeed.enabled()),
        }
    }

    pub fn set_effect_enabled(&mut self, effect: FxEffect, on: bool) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_enabled(on),
            FxEffect::Ambience => self.ambience.set_enabled(on),
            FxEffect::Surround => self.surround.set_enabled(on),
            FxEffect::DynamicBoost => self.dynamic_boost.set_enabled(on),
            FxEffect::Bass => self.bass.set_enabled(on),
            FxEffect::Loudness => self.loudness.set_enabled(on),
            FxEffect::Crossfeed => self.crossfeed.set_enabled(on),
        }
    }

    pub fn reset(&mut self) {
        self.filters
            .iter_mut()
            .for_each(super::filter::ApoFilter::reset);
        self.convolution.reset();
        self.eq_fir.reset();
        // A reset is a hard restart: finalise any pending path transition so
        // the chain starts on the desired path at full gain.
        self.fir_active = self.phase_mode == PhaseMode::Linear
            && self.eq_fir.enabled()
            && self.eq_fir.source().is_some();
        self.fir_kernel_id = self
            .eq_fir
            .source()
            .map_or(0, |a| std::sync::Arc::as_ptr(a).cast::<u8>() as usize);
        self.fir_fade = FirFade::Stable;
        self.fir_gain = 1.0;
        self.fir_in_gain = 1.0;
        self.fidelity.reset();
        self.ambience.reset();
        self.surround.reset();
        self.dynamic_boost.reset();
        self.bass.reset();
        self.loudness.reset();
        self.crossfeed.reset();
    }

    /// Rebind every sample-rate-dependent coefficient to a new output rate.
    ///
    /// A device/format change (e.g. switching outputs) renegotiates the rate,
    /// which invalidates not just the biquad filters but the effects too (their
    /// internal filters and reverb delays are rate-derived). Filters are updated
    /// in place; effects are rebuilt at the new rate, carrying over intensity +
    /// enabled (their sample history resets, which is unavoidable on a rate
    /// change). No-op when the rate is unchanged.
    // float_cmp: exact compare of stored rate vs incoming is the no-op guard.
    #[allow(clippy::float_cmp)]
    pub fn rebind_sample_rate(&mut self, sample_rate: f64) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        for f in &mut self.filters {
            // A band whose freq is at/above the new Nyquist (e.g. a 20 kHz band
            // after 48k→32k) can't be realized — `rebind` holds it inert rather
            // than leaving the old-rate coefficients live, and re-arms it on its
            // own if a later rate makes it realizable again. User `enabled`
            // intent is untouched.
            f.rebind(sample_rate);
        }
        // Re-prepare the convolution kernel from its retained source IR at the
        // new rate (no-op when nothing is loaded).
        self.convolution.rebind_sample_rate(sample_rate);
        // The FIR grid is rate-derived; re-prepare from the retained kernel so
        // audio stays correct until the daemon re-renders at the new grid.
        self.eq_fir.rebind_sample_rate(sample_rate);
        let ch = self.channels;
        self.fidelity = carry_settings(&self.fidelity, FidelityEffect::new(ch, sample_rate));
        self.ambience = carry_settings(&self.ambience, AmbienceEffect::new(ch, sample_rate));
        self.surround = carry_settings(&self.surround, SurroundEffect::new(sample_rate));
        self.dynamic_boost =
            carry_settings(&self.dynamic_boost, DynamicBoostEffect::new(sample_rate));
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(ch, sample_rate));
        self.loudness = carry_settings(&self.loudness, LoudnessEffect::new(ch, sample_rate));
        self.crossfeed = carry_settings(&self.crossfeed, CrossfeedEffect::new(ch, sample_rate));
    }

    /// Rebind every channel-count-dependent buffer to a new processing channel
    /// count (device renegotiation: stereo → 5.1, mono SCO → stereo A2DP, …).
    ///
    /// Filter per-channel state is resized in place (kept channels keep history);
    /// effects are rebuilt at the new count, carrying intensity + enabled (their
    /// sample history resets, unavoidable on a layout change). Band channel masks
    /// are untouched — a band targeting channel 0 still targets channel 0. No-op
    /// when the count is unchanged or zero.
    pub fn set_channels(&mut self, channels: usize) {
        if channels == 0 || self.channels == channels {
            return;
        }
        self.channels = channels;
        for f in &mut self.filters {
            f.set_channels(channels);
        }
        self.convolution.set_channels(channels);
        self.eq_fir.set_channels(channels);
        let sr = self.sample_rate;
        self.fidelity = carry_settings(&self.fidelity, FidelityEffect::new(channels, sr));
        self.ambience = carry_settings(&self.ambience, AmbienceEffect::new(channels, sr));
        self.surround = carry_settings(&self.surround, SurroundEffect::new(sr));
        self.dynamic_boost = carry_settings(&self.dynamic_boost, DynamicBoostEffect::new(sr));
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(channels, sr));
        self.loudness = carry_settings(&self.loudness, LoudnessEffect::new(channels, sr));
        self.crossfeed = carry_settings(&self.crossfeed, CrossfeedEffect::new(channels, sr));
        self.dither.set_channels(channels);
    }

    /// The channel count the chain emits: the routing matrix's output width when
    /// one is set, otherwise the processing channel count.
    pub fn out_channels(&self) -> usize {
        self.routing
            .as_ref()
            .map_or(self.channels, ChannelMatrix::out_ch)
    }

    /// Apply the output routing matrix, writing `processed` (the in-place result
    /// of [`ProcessorChain::process`], `frames * channels` interleaved) into
    /// `out` (`frames * out_channels()`). With no matrix — or a square identity —
    /// this is a straight copy, the zero-cost common path. Allocation-free; the
    /// caller owns `out` and sizes it to `out_channels()`.
    pub fn route(&self, processed: &[f64], out: &mut [f64]) {
        match &self.routing {
            Some(m) if !m.is_identity() => m.apply(processed, out),
            _ => {
                let n = processed.len().min(out.len());
                out[..n].copy_from_slice(&processed[..n]);
            }
        }
    }
}

/// Copy intensity + enabled from an existing effect onto a freshly-built one.
fn carry_settings<E: Effect>(old: &E, mut fresh: E) -> E {
    fresh.set_intensity(old.intensity());
    fresh.set_enabled(old.enabled());
    fresh
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Build the Listen-mode audition filter for `band`: a unity-gain filter that
/// isolates the band's operating region, using an existing `FilterType`.
/// Peaking-style bands → band-pass at Fc/Q; shelves + pass filters → a plain
/// (Butterworth) low-/high-pass at Fc. `None` if the coefficients are
/// unrealizable at `sr` (Listen then simply bypasses all bands).
fn build_audition_filter(band: &ApoFilter, channels: usize, sr: f64) -> Option<ApoFilter> {
    use crate::filter::FilterType::{
        AllPass, BandPass, HighPass, HighPassQ, HighShelf, HighShelf12Db, HighShelfQ, LowPass,
        LowPassQ, LowShelf, LowShelf12Db, LowShelfQ, Notch, Peaking,
    };
    let (ft, q) = match band.filter_type {
        Peaking | Notch | AllPass | BandPass => (BandPass, band.q),
        LowShelf | LowShelf12Db | LowShelfQ | LowPass | LowPassQ => {
            (LowPass, std::f64::consts::FRAC_1_SQRT_2)
        }
        HighShelf | HighShelf12Db | HighShelfQ | HighPass | HighPassQ => {
            (HighPass, std::f64::consts::FRAC_1_SQRT_2)
        }
    };
    ApoFilter::builder()
        .filter_type(ft)
        .freq(band.freq)
        .gain_db(0.0)
        .q(q)
        .enabled(true)
        .channels(channels)
        .sample_rate(sr)
        .build()
        .ok()
}

#[derive(Debug)]
pub struct ProcessorChainBuilder {
    channels: usize,
    sample_rate: f64,
    preamp_db: f64,
    filters: Vec<ApoFilter>,
}

impl Default for ProcessorChainBuilder {
    fn default() -> Self {
        Self {
            channels: 2,
            sample_rate: 48000.0,
            preamp_db: 0.0,
            filters: Vec::new(),
        }
    }
}

impl ProcessorChainBuilder {
    #[must_use]
    pub fn channels(mut self, n: usize) -> Self {
        self.channels = n;
        self
    }

    #[must_use]
    pub fn sample_rate(mut self, sr: f64) -> Self {
        self.sample_rate = sr;
        self
    }

    #[must_use]
    pub fn preamp_db(mut self, db: f64) -> Self {
        self.preamp_db = db;
        self
    }

    #[must_use]
    pub fn add_filter(mut self, filter: ApoFilter) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn build(self) -> ProcessorChain {
        let channels = self.channels;
        let sr = self.sample_rate;
        ProcessorChain {
            channels,
            sample_rate: sr,
            enabled: true,
            preamp_db: self.preamp_db,
            filters: self.filters,
            convolution: ConvolutionEngine::new(channels, sr),
            phase_mode: PhaseMode::default(),
            eq_fir: ConvolutionEngine::new(channels, sr),
            fir_active: false,
            fir_kernel_id: 0,
            fir_fade: FirFade::default(),
            fir_gain: 1.0,
            fir_in_gain: 1.0,
            fidelity: FidelityEffect::new(channels, sr),
            ambience: AmbienceEffect::new(channels, sr),
            surround: SurroundEffect::new(sr),
            dynamic_boost: DynamicBoostEffect::new(sr),
            bass: BassBoostEffect::new(channels, sr),
            loudness: LoudnessEffect::new(channels, sr),
            crossfeed: CrossfeedEffect::new(channels, sr),
            dither: DitherStage::new(channels),
            routing: None,
            audition: None,
            audition_filter: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_disabled() {
        let mut chain = ProcessorChain::builder().build();
        chain.enabled = false;
        let input = vec![0.5f64; 64];
        let mut buf = input.clone();
        chain.process(&mut buf);
        assert_eq!(buf, input);
    }

    #[test]
    fn passthrough_when_no_effects() {
        let mut chain = ProcessorChain::builder().build();
        let input = vec![0.5f64; 64];
        let mut buf = input.clone();
        chain.process(&mut buf);
        assert_eq!(buf, input);
    }

    #[test]
    // float_cmp: asserts the stored rate equals the exact rate just set.
    #[allow(clippy::float_cmp)]
    fn rebind_sample_rate_updates_rate_and_preserves_effect_settings() {
        use crate::filter::{ApoFilter, FilterType};
        let mut chain = ProcessorChain::builder()
            .sample_rate(48_000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(1_000.0)
                    .gain_db(6.0)
                    .q(2.0)
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build()
                    .unwrap(),
            )
            .build();
        chain.set_effect_intensity(FxEffect::Bass, 0.7);
        chain.set_effect_enabled(FxEffect::Bass, true);

        chain.rebind_sample_rate(44_100.0);

        assert_eq!(chain.sample_rate, 44_100.0);
        // Effect intensity + enabled carried across the rate change.
        assert!((chain.bass.intensity() - 0.7).abs() < 1e-9);
        assert!(chain.bass.enabled());
        // The filter still describes the same band at the new rate.
        assert!((chain.filters[0].freq - 1_000.0).abs() < 1e-9);
        assert!((chain.filters[0].gain_db - 6.0).abs() < 1e-9);

        // No-op when unchanged.
        chain.rebind_sample_rate(44_100.0);
        assert_eq!(chain.sample_rate, 44_100.0);
    }

    #[test]
    fn solo_isolates_a_single_band() {
        use crate::filter::FilterType;
        // Two well-separated +12 dB peaks. Soloing one must leave the *other*
        // band's frequency untouched (~unity) while the soloed band's own
        // frequency stays boosted.
        let mk = || {
            ProcessorChain::builder()
                .channels(1)
                .sample_rate(48_000.0)
                .add_filter(band(FilterType::Peaking, 200.0, 12.0, 1.0))
                .add_filter(band(FilterType::Peaking, 5_000.0, 12.0, 1.0))
                .build()
        };

        // Steady-state RMS gain of a pure sine through the chain.
        let gain_at = |chain: &mut ProcessorChain, hz: f64| -> f64 {
            let n = 8_192usize;
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            #[allow(clippy::cast_precision_loss)]
            let mut buf: Vec<f64> = (0..n).map(|i| (w * i as f64).sin() * 0.5).collect();
            let input = buf.clone();
            chain.process(&mut buf);
            // Skip the biquad warm-up transient.
            let rms = |s: &[f64]| (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt();
            rms(&buf[2_048..]) / rms(&input[2_048..])
        };

        // Solo band 0 (200 Hz): 200 Hz boosted, 5 kHz ~unity.
        let mut c = mk();
        c.set_audition(Some(BandAudition {
            band: 0,
            mode: AuditionMode::Solo,
        }));
        assert!(
            gain_at(&mut c, 200.0) > 3.0,
            "soloed 200 Hz band lost its boost"
        );
        let off = gain_at(&mut c, 5_000.0);
        assert!(
            (off - 1.0).abs() < 0.15,
            "5 kHz leaked while band 0 soloed: {off}"
        );

        // Solo band 1 (5 kHz): mirror.
        let mut c = mk();
        c.set_audition(Some(BandAudition {
            band: 1,
            mode: AuditionMode::Solo,
        }));
        assert!(
            gain_at(&mut c, 5_000.0) > 3.0,
            "soloed 5 kHz band lost its boost"
        );
        let off = gain_at(&mut c, 200.0);
        assert!(
            (off - 1.0).abs() < 0.15,
            "200 Hz leaked while band 1 soloed: {off}"
        );

        // No solo: both bands active — each frequency is boosted.
        let mut c = mk();
        assert!(gain_at(&mut c, 200.0) > 3.0);
        assert!(gain_at(&mut c, 5_000.0) > 3.0);

        // Clearing solo restores the full cascade.
        let mut c = mk();
        c.set_audition(Some(BandAudition {
            band: 0,
            mode: AuditionMode::Solo,
        }));
        assert!((gain_at(&mut c, 5_000.0) - 1.0).abs() < 0.15);
        c.set_audition(None);
        assert!(
            gain_at(&mut c, 5_000.0) > 3.0,
            "clearing solo did not restore band 1"
        );
    }

    #[test]
    fn listen_bandpasses_a_peaking_band() {
        use crate::filter::FilterType;
        // A +12 dB peak at 1 kHz. In Listen the +12 is irrelevant (unity BP) —
        // the point is: energy survives near 1 kHz, far probes are killed.
        let gain_at = |chain: &mut ProcessorChain, hz: f64| -> f64 {
            let n = 8_192usize;
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            #[allow(clippy::cast_precision_loss)]
            let mut buf: Vec<f64> = (0..n).map(|i| (w * i as f64).sin() * 0.5).collect();
            let input = buf.clone();
            chain.process(&mut buf);
            let rms = |s: &[f64]| (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt();
            rms(&buf[2_048..]) / rms(&input[2_048..])
        };

        let mut c = ProcessorChain::builder()
            .channels(1)
            .sample_rate(48_000.0)
            .add_filter(band(FilterType::Peaking, 1_000.0, 12.0, 2.0))
            .build();
        c.set_audition(Some(BandAudition {
            band: 0,
            mode: AuditionMode::Listen,
        }));
        // In-band passes ~unity; far out-of-band is strongly attenuated by the BP.
        assert!(
            gain_at(&mut c, 1_000.0) > 0.5,
            "1 kHz should pass in Listen"
        );
        assert!(
            gain_at(&mut c, 100.0) < 0.2,
            "100 Hz should be cut by the BP"
        );
        assert!(
            gain_at(&mut c, 10_000.0) < 0.2,
            "10 kHz should be cut by the BP"
        );
    }

    #[test]
    fn listen_low_shelf_uses_low_pass() {
        use crate::filter::FilterType;
        let gain_at = |chain: &mut ProcessorChain, hz: f64| -> f64 {
            let n = 8_192usize;
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            #[allow(clippy::cast_precision_loss)]
            let mut buf: Vec<f64> = (0..n).map(|i| (w * i as f64).sin() * 0.5).collect();
            let input = buf.clone();
            chain.process(&mut buf);
            let rms = |s: &[f64]| (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt();
            rms(&buf[2_048..]) / rms(&input[2_048..])
        };
        let mut c = ProcessorChain::builder()
            .channels(1)
            .sample_rate(48_000.0)
            .add_filter(band(FilterType::LowShelf, 500.0, 6.0, 0.707))
            .build();
        c.set_audition(Some(BandAudition {
            band: 0,
            mode: AuditionMode::Listen,
        }));
        assert!(
            gain_at(&mut c, 100.0) > 0.7,
            "low shelf → LP: 100 Hz passes"
        );
        assert!(gain_at(&mut c, 8_000.0) < 0.2, "low shelf → LP: 8 kHz cut");
    }

    fn band(ft: crate::filter::FilterType, f: f64, g: f64, q: f64) -> crate::filter::ApoFilter {
        crate::filter::ApoFilter::builder()
            .filter_type(ft)
            .freq(f)
            .gain_db(g)
            .q(q)
            .enabled(true)
            .channels(1)
            .sample_rate(48_000.0)
            .build()
            .unwrap()
    }

    #[test]
    fn multi_band_cascade_is_order_equivalent() {
        use crate::filter::{ApoFilter, FilterType};
        // The sample-major cascade must produce bit-identical output to applying
        // each band as a full sequential pass (filter-major), the obviously-
        // correct reference.
        let specs = [
            (FilterType::Peaking, 100.0, 4.0, 1.0),
            (FilterType::HighShelf, 8000.0, -3.0, 0.707),
            (FilterType::Peaking, 1000.0, -5.0, 2.0),
        ];
        let mk = || {
            let mut b = ProcessorChain::builder().channels(2).sample_rate(48_000.0);
            for (ft, f, g, q) in specs {
                b = b.add_filter(
                    ApoFilter::builder()
                        .filter_type(ft)
                        .freq(f)
                        .gain_db(g)
                        .q(q)
                        .enabled(true)
                        .channels(2)
                        .sample_rate(48_000.0)
                        .build()
                        .unwrap(),
                );
            }
            b.build()
        };
        let input: Vec<f64> = (0..512)
            .map(|i| (f64::from(i) * 0.017).sin() * 0.6)
            .collect();

        // Reference: each band applied as its own full pass (filter-major).
        let mut reference = input.clone();
        let mut ref_chain = mk();
        for fi in 0..ref_chain.filters.len() {
            for frame in 0..(reference.len() / 2) {
                for ch in 0..2 {
                    let idx = frame * 2 + ch;
                    reference[idx] = ref_chain.filters[fi].process_channel(reference[idx], ch);
                }
            }
        }

        let mut got = input.clone();
        mk().process(&mut got);

        for (a, b) in reference.iter().zip(&got) {
            assert_eq!(a.to_bits(), b.to_bits(), "cascade reorder changed output");
        }
    }

    #[test]
    fn rebind_holds_unrealizable_band_inert_then_re_arms() {
        use crate::filter::{ApoFilter, FilterType};
        // A 20 kHz band is realizable at 48k (Nyquist 24k) but not at 32k
        // (Nyquist 16k). It must go inert at 32k yet keep processing again at 48k
        // — and the user-facing `enabled` flag stays set throughout.
        let mut chain = ProcessorChain::builder()
            .sample_rate(48_000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(20_000.0)
                    .gain_db(6.0)
                    .q(2.0)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build()
                    .unwrap(),
            )
            .build();

        let probe = |c: &mut ProcessorChain| {
            c.reset();
            let mut buf = vec![0.5; 64];
            c.process(&mut buf);
            buf.iter().any(|&s| (s - 0.5).abs() > 1e-9)
        };

        assert!(probe(&mut chain), "band should process at 48k");
        chain.rebind_sample_rate(32_000.0);
        assert!(chain.filters[0].enabled, "user enabled flag preserved");
        assert!(!probe(&mut chain), "band inert at 32k (above Nyquist)");
        chain.rebind_sample_rate(48_000.0);
        assert!(probe(&mut chain), "band re-arms when the rate returns");
    }

    #[test]
    fn preamp_applies_exact_gain() {
        let mut chain = ProcessorChain::builder().preamp_db(6.0).build();
        let gain = 10f64.powf(6.0 / 20.0);
        let input = vec![0.1, -0.2, 0.3, -0.4];
        let mut buf = input.clone();
        chain.process(&mut buf);
        for (i, o) in input.iter().zip(&buf) {
            assert!(
                (o - i * gain).abs() < 1e-12,
                "preamp gain mismatch: {o} vs {}",
                i * gain
            );
        }
    }

    #[test]
    fn chain_dither_quantizes_output_when_enabled() {
        // With dither on, the chain's output must land on the target grid; with
        // it off (the default) the chain stays a bit-exact passthrough (covered
        // by `full_default_chain_is_bit_perfect_passthrough`).
        let mut chain = ProcessorChain::builder().channels(2).build();
        chain.set_dither(Some(16));
        let q = 1.0 / f64::from(1u32 << 15);
        let mut buf: Vec<f64> = (0..256)
            .map(|i| (f64::from(i) * 0.05).sin() * 0.4)
            .collect();
        chain.process(&mut buf);
        assert!(
            buf.iter().all(|&y| ((y / q).round() - y / q).abs() < 1e-6),
            "chain output should be quantised to the grid when dither is on"
        );
    }

    #[test]
    fn full_default_chain_is_bit_perfect_passthrough() {
        // Default chain: no filters, all effects at 0 intensity, preamp 0.
        // Must pass audio through bit-for-bit.
        let mut chain = ProcessorChain::builder().build();
        let input: Vec<f64> = (0..256)
            .map(|i| (f64::from(i) * 0.013).sin() * 0.7)
            .collect();
        let mut buf = input.clone();
        chain.process(&mut buf);
        assert_eq!(buf, input);
    }

    // ── Mid/side band scope ─────────────────────────────────────────────────

    fn ms_chain(scope: crate::filter::BandScope) -> ProcessorChain {
        use crate::filter::{ApoFilter, FilterType};
        ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(1_000.0)
                    .gain_db(12.0)
                    .q(1.0)
                    .scope(scope)
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build()
                    .unwrap(),
            )
            .build()
    }

    fn tone_1k(frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|i| (2.0 * std::f64::consts::PI * 1_000.0 / 48_000.0 * i as f64).sin() * 0.5)
            .collect()
    }

    fn ch_rms(buf: &[f64], ch: usize) -> f64 {
        let v: Vec<f64> = buf.iter().skip(ch).step_by(2).copied().collect();
        (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt()
    }

    fn side_rms(buf: &[f64]) -> f64 {
        let sd: Vec<f64> = buf.chunks(2).map(|f| (f[0] - f[1]) / 2.0).collect();
        (sd.iter().map(|x| x * x).sum::<f64>() / sd.len() as f64).sqrt()
    }

    #[test]
    fn mid_band_ignores_pure_side_signal() {
        use crate::filter::BandScope;
        // A pure side signal (L = +x, R = −x) has zero mid content, so a
        // Mid-scoped band must leave it untouched.
        let mut chain = ms_chain(BandScope::Mid);
        let s = tone_1k(2048);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, -x]).collect();
        let input = buf.clone();
        chain.process(&mut buf);
        for (a, b) in input.iter().zip(&buf) {
            assert!(
                (a - b).abs() < 1e-9,
                "mid band must not touch a pure side signal"
            );
        }
    }

    #[test]
    fn mid_band_boosts_mono_signal() {
        use crate::filter::BandScope;
        // A mono signal is pure mid, so a +12 dB Mid band at the tone frequency
        // must boost it.
        let mut chain = ms_chain(BandScope::Mid);
        let s = tone_1k(4096);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        let before = ch_rms(&buf, 0);
        chain.process(&mut buf);
        let after = ch_rms(&buf, 0);
        assert!(
            after > before * 2.0,
            "mid band should boost a mono signal: {before:.3} → {after:.3}"
        );
    }

    #[test]
    fn side_band_ignores_mono_signal() {
        use crate::filter::BandScope;
        // A mono signal has zero side content, so a Side-scoped band leaves it be.
        let mut chain = ms_chain(BandScope::Side);
        let s = tone_1k(2048);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        let input = buf.clone();
        chain.process(&mut buf);
        for (a, b) in input.iter().zip(&buf) {
            assert!(
                (a - b).abs() < 1e-9,
                "side band must not touch a mono signal"
            );
        }
    }

    #[test]
    fn side_band_boosts_pure_side_signal() {
        use crate::filter::BandScope;
        let mut chain = ms_chain(BandScope::Side);
        let s = tone_1k(4096);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, -x]).collect();
        let before = side_rms(&buf);
        chain.process(&mut buf);
        let after = side_rms(&buf);
        assert!(
            after > before * 2.0,
            "side band should boost a pure side signal: {before:.3} → {after:.3}"
        );
    }

    #[test]
    fn stereo_scope_boosts_both_channels_equally() {
        use crate::filter::BandScope;
        // The default Stereo scope processes each channel independently — a mono
        // input comes out boosted equally on both channels.
        let mut chain = ms_chain(BandScope::Stereo);
        let s = tone_1k(4096);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        let before = ch_rms(&buf, 0);
        chain.process(&mut buf);
        let (l, r) = (ch_rms(&buf, 0), ch_rms(&buf, 1));
        assert!(
            l > before * 2.0 && (l - r).abs() < 1e-6,
            "stereo band should boost both channels equally: L {l:.3} R {r:.3}"
        );
    }

    // ── Dynamic EQ in the chain ─────────────────────────────────────────────

    /// A 1 kHz Peaking band (gain 0) with a −6 dB dynamics cut engaging at
    /// −25 dBFS, plus the given scope/mask.
    fn dyn_chain(
        scope: crate::filter::BandScope,
        mask: crate::channel::ChannelMask,
    ) -> ProcessorChain {
        use crate::filter::{ApoFilter, DynParams, FilterType};
        ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(1_000.0)
                    .gain_db(0.0)
                    .q(1.0)
                    .scope(scope)
                    .channel_mask(mask)
                    .dynamics(Some(DynParams {
                        threshold_db: -25.0,
                        range_db: -6.0,
                        attack_ms: 2.0,
                        release_ms: 150.0,
                    }))
                    .enabled(true)
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build()
                    .unwrap(),
            )
            .build()
    }

    /// RMS of one interleaved-stereo channel over the last quarter (settled).
    fn settled_ch_rms(buf: &[f64], ch: usize) -> f64 {
        let tail = &buf[buf.len() * 3 / 4..];
        ch_rms(tail, ch)
    }

    #[test]
    fn dyn_band_in_chain_cuts_when_loud() {
        use crate::channel::ChannelMask;
        use crate::filter::BandScope;
        // A −6 dBFS mono 1 kHz tone through the dynamic band settles ~6 dB
        // below the input level on both channels.
        let mut chain = dyn_chain(BandScope::Stereo, ChannelMask::ALL);
        let s = tone_1k(16384);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        let before = settled_ch_rms(&buf, 0);
        chain.process(&mut buf);
        let after = settled_ch_rms(&buf, 0);
        let g = 20.0 * (after / before).log10();
        assert!((g - (-6.0)).abs() < 0.5, "chain dyn cut: got {g:.2} dB");
        let r = settled_ch_rms(&buf, 1);
        assert!(
            (after - r).abs() < 1e-9,
            "linked detector must cut both channels equally"
        );
    }

    #[test]
    fn dyn_side_scope_triggers_on_side_only() {
        use crate::channel::ChannelMask;
        use crate::filter::BandScope;
        // Pure-side loud signal engages a Side-scoped dynamic band...
        let mut chain = dyn_chain(BandScope::Side, ChannelMask::ALL);
        let s = tone_1k(16384);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, -x]).collect();
        let before = settled_ch_rms(&buf, 0);
        chain.process(&mut buf);
        let after = settled_ch_rms(&buf, 0);
        let g = 20.0 * (after / before).log10();
        assert!((g - (-6.0)).abs() < 0.5, "side dyn cut: got {g:.2} dB");

        // ...while a loud mono signal (zero side content) leaves it untouched.
        let mut chain = dyn_chain(BandScope::Side, ChannelMask::ALL);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        let input = buf.clone();
        chain.process(&mut buf);
        for (a, b) in input.iter().zip(&buf) {
            assert!((a - b).abs() < 1e-9, "side dyn band must not react to mono");
        }
    }

    #[test]
    fn dyn_masked_band_ignores_unmasked_channel() {
        use crate::channel::ChannelMask;
        use crate::filter::BandScope;
        // Band masked to ch0; the loud tone lives only on ch1 → the linked
        // detector (masked channels only) must never trigger, and ch0's quiet
        // in-band tone passes at unity.
        let mask = ChannelMask::from_bits(0b01);
        let mut chain = dyn_chain(BandScope::Stereo, mask);
        let s = tone_1k(16384);
        // ch0 quiet (−52 dBFS), ch1 loud (−6 dBFS)
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x * 0.005, x]).collect();
        let input = buf.clone();
        chain.process(&mut buf);
        let g0 = 20.0 * (settled_ch_rms(&buf, 0) / settled_ch_rms(&input, 0)).log10();
        assert!(
            g0.abs() < 0.1,
            "masked dyn band must not trigger off an unmasked channel: {g0:.2} dB"
        );
        // ch1 is outside the mask — bit-untouched.
        for (i, (a, b)) in input.iter().zip(&buf).enumerate() {
            if i % 2 == 1 {
                assert_eq!(a.to_bits(), b.to_bits(), "unmasked channel changed");
            }
        }
    }

    #[test]
    fn static_bands_bit_exact_with_dynamics_feature_present() {
        use crate::filter::{ApoFilter, FilterType};
        // The static band-major path must stay byte-identical to the reference
        // now that a dynamics branch exists in the loop.
        let mk = || {
            ProcessorChain::builder()
                .channels(2)
                .sample_rate(48_000.0)
                .add_filter(
                    ApoFilter::builder()
                        .filter_type(FilterType::Peaking)
                        .freq(1_000.0)
                        .gain_db(-5.0)
                        .q(2.0)
                        .enabled(true)
                        .channels(2)
                        .sample_rate(48_000.0)
                        .build()
                        .unwrap(),
                )
                .build()
        };
        let input: Vec<f64> = (0..2048)
            .map(|i| (f64::from(i) * 0.017).sin() * 0.6)
            .collect();
        let mut reference = input.clone();
        let mut rc = mk();
        for frame in 0..(reference.len() / 2) {
            for ch in 0..2 {
                let idx = frame * 2 + ch;
                reference[idx] = rc.filters[0].process_channel(reference[idx], ch);
            }
        }
        let mut got = input.clone();
        mk().process(&mut got);
        for (a, b) in reference.iter().zip(&got) {
            assert_eq!(a.to_bits(), b.to_bits(), "static path changed");
        }
    }

    // ── Linear-phase mode ───────────────────────────────────────────────────

    /// Build a stereo chain with the given bands, render + load the FIR and
    /// switch to Linear.
    fn linear_chain(bands: Vec<crate::filter::ApoFilter>) -> ProcessorChain {
        let mut b = ProcessorChain::builder().channels(2).sample_rate(48_000.0);
        for f in bands {
            b = b.add_filter(f);
        }
        let mut chain = b.build();
        let ir = crate::linphase::render(&chain.filters, 2, 48_000.0).expect("kernel");
        chain.eq_fir.load_ir(std::sync::Arc::new(ir)).expect("load");
        chain.set_phase_mode(PhaseMode::Linear);
        // Start settled on the FIR path (a fresh daemon chain, not a live
        // mid-stream toggle — those are covered by the click-free tests).
        chain.reset();
        chain
    }

    fn peak_band(freq: f64, gain_db: f64, q: f64) -> crate::filter::ApoFilter {
        crate::filter::ApoFilter::builder()
            .filter_type(crate::filter::FilterType::Peaking)
            .freq(freq)
            .gain_db(gain_db)
            .q(q)
            .enabled(true)
            .channels(2)
            .sample_rate(48_000.0)
            .build()
            .unwrap()
    }

    #[test]
    fn linear_mode_delay_is_frequency_independent() {
        // An impulse through the FIR bank peaks exactly at BLOCK + N/2 no
        // matter where the band sits — constant group delay = linear phase.
        for band_freq in [200.0, 8_000.0] {
            let mut chain = linear_chain(vec![peak_band(band_freq, 6.0, 1.0)]);
            let n = crate::linphase::grid_len(48_000.0);
            let frames = n + 1024;
            let mut buf = vec![0.0f64; frames * 2];
            buf[0] = 1.0;
            buf[1] = 1.0;
            chain.process(&mut buf);
            let argmax = (0..frames)
                .max_by(|&a, &b| buf[a * 2].abs().total_cmp(&buf[b * 2].abs()))
                .unwrap();
            let expected = crate::convolution::BLOCK + n / 2;
            assert_eq!(
                argmax, expected,
                "peak for the {band_freq} Hz band must sit at BLOCK + N/2"
            );
        }
    }

    #[test]
    fn linear_mode_without_kernel_falls_back_to_iir() {
        // Mode set but no kernel loaded (render pending/failed): the IIR path
        // must keep running — never a silent/flat gap.
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(peak_band(1_000.0, 12.0, 1.0))
            .build();
        chain.set_phase_mode(PhaseMode::Linear);
        let s = tone_1k(4096);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        let before = ch_rms(&buf, 0);
        chain.process(&mut buf);
        assert!(
            ch_rms(&buf, 0) > before * 2.0,
            "iir band must still apply while no kernel is loaded"
        );
    }

    #[test]
    fn linear_mode_keeps_dynamic_bands_on_iir() {
        use crate::filter::DynParams;
        // Hybrid: the static band is linearised, the dynamic band still morphs.
        let mut dynamic = peak_band(1_000.0, 0.0, 1.0);
        dynamic
            .set_dynamics(
                Some(DynParams {
                    threshold_db: -25.0,
                    range_db: -6.0,
                    attack_ms: 2.0,
                    release_ms: 150.0,
                }),
                48_000.0,
            )
            .unwrap();
        let mut chain = linear_chain(vec![peak_band(150.0, 6.0, 1.0), dynamic]);
        let n = crate::linphase::grid_len(48_000.0);
        let s = tone_1k(16_384 + n);
        let mut buf: Vec<f64> = s.iter().flat_map(|&x| [x, x]).collect();
        chain.process(&mut buf);
        // Measure well past the FIR latency: the loud 1 kHz tone must still be
        // cut ~6 dB by the dynamic band (which the kernel must NOT contain).
        let tail = &buf[buf.len() * 3 / 4..];
        let g = 20.0 * (ch_rms(tail, 0) / (0.5 / std::f64::consts::SQRT_2)).log10();
        assert!(
            (g - (-6.0)).abs() < 0.7,
            "dynamic band must keep morphing in linear mode: {g:.2} dB"
        );
    }

    #[test]
    fn leaving_linear_mode_returns_bit_exact_minimum_path() {
        let mk = || {
            ProcessorChain::builder()
                .channels(2)
                .sample_rate(48_000.0)
                .add_filter(peak_band(1_000.0, 6.0, 1.0))
                .build()
        };
        let input: Vec<f64> = (0..4096)
            .map(|i| (f64::from(i) * 0.013).sin() * 0.5)
            .collect();

        let mut reference = mk();
        let mut a = input.clone();
        reference.process(&mut a);

        let mut toggled = mk();
        let ir = crate::linphase::render(&toggled.filters, 2, 48_000.0).unwrap();
        toggled.eq_fir.load_ir(std::sync::Arc::new(ir)).unwrap();
        toggled.set_phase_mode(PhaseMode::Linear);
        let mut warm = input.clone();
        toggled.process(&mut warm);
        toggled.set_phase_mode(PhaseMode::Minimum);
        toggled.reset();
        reference.reset();

        let mut b = input.clone();
        toggled.process(&mut b);
        let mut a2 = input.clone();
        reference.process(&mut a2);
        for (x, y) in a2.iter().zip(&b) {
            assert_eq!(
                x.to_bits(),
                y.to_bits(),
                "minimum path changed after toggle"
            );
        }
    }

    #[test]
    fn eq_fir_latency_reported_only_when_active() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(peak_band(1_000.0, 6.0, 1.0))
            .build();
        assert_eq!(chain.eq_fir_latency_frames(), 0);
        let ir = crate::linphase::render(&chain.filters, 2, 48_000.0).unwrap();
        chain.eq_fir.load_ir(std::sync::Arc::new(ir)).unwrap();
        chain.set_phase_mode(PhaseMode::Linear);
        let n = crate::linphase::grid_len(48_000.0);
        assert_eq!(
            chain.eq_fir_latency_frames(),
            crate::convolution::BLOCK + n / 2
        );
        chain.set_phase_mode(PhaseMode::Minimum);
        assert_eq!(chain.eq_fir_latency_frames(), 0);
    }

    // ── Click-free phase-mode transitions ───────────────────────────────────

    /// Feed a continuous low-frequency sine block-by-block, flipping something
    /// mid-stream; the largest sample-to-sample step in the output bounds the
    /// audible click. A 200 Hz sine at 0.5 amp moves ≤ ~0.014/sample on its
    /// own, so anything ≫ that is a discontinuity.
    fn max_step_across(mut chain: ProcessorChain, flip: impl FnOnce(&mut ProcessorChain)) -> f64 {
        let frames = 512;
        let blocks = 40;
        let mut phase = 0.0f64;
        let dp = 2.0 * std::f64::consts::PI * 200.0 / 48_000.0;
        let mut prev = 0.0f64;
        let mut max_step = 0.0f64;
        let mut flip = Some(flip);
        for blk in 0..blocks {
            let mut buf = Vec::with_capacity(frames * 2);
            for _ in 0..frames {
                let x = 0.5 * phase.sin();
                phase += dp;
                buf.push(x);
                buf.push(x);
            }
            if blk == 10 {
                if let Some(f) = flip.take() {
                    f(&mut chain);
                }
            }
            chain.process(&mut buf);
            for fr in 0..frames {
                let y = buf[fr * 2];
                max_step = max_step.max((y - prev).abs());
                prev = y;
            }
        }
        max_step
    }

    #[test]
    fn entering_linear_mode_is_click_free() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(peak_band(1_000.0, 12.0, 1.0))
            .build();
        let ir = crate::linphase::render(&chain.filters, 2, 48_000.0).unwrap();
        chain.eq_fir.load_ir(std::sync::Arc::new(ir)).unwrap();
        // still in Minimum; the flip switches to Linear mid-stream
        let step = max_step_across(chain, |c| c.set_phase_mode(PhaseMode::Linear));
        assert!(step < 0.05, "entering linear clicked: max step {step:.3}");
    }

    #[test]
    fn leaving_linear_mode_is_click_free() {
        let chain = linear_chain(vec![peak_band(1_000.0, 12.0, 1.0)]);
        let step = max_step_across(chain, |c| c.set_phase_mode(PhaseMode::Minimum));
        assert!(step < 0.05, "leaving linear clicked: max step {step:.3}");
    }

    #[test]
    fn kernel_swap_while_linear_is_click_free() {
        // A band edit in linear mode swaps in a fresh kernel (new engine
        // history) — must also ride the fade.
        let chain = linear_chain(vec![peak_band(1_000.0, 12.0, 1.0)]);
        let step = max_step_across(chain, |c| {
            let mut f2 = vec![peak_band(2_000.0, 6.0, 2.0)];
            let ir = crate::linphase::render(&f2, 2, 48_000.0).unwrap();
            let mut fresh = ConvolutionEngine::new(2, 48_000.0);
            fresh.load_ir(std::sync::Arc::new(ir)).unwrap();
            c.eq_fir = fresh;
            let _ = &mut f2;
        });
        assert!(step < 0.05, "kernel swap clicked: max step {step:.3}");
    }
}
