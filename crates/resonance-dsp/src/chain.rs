use crate::effects::{
    AmbienceEffect, BassBoostEffect, DynamicBoostEffect, Effect, FidelityEffect, SurroundEffect,
};
use crate::filter::ApoFilter;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FxEffect {
    Fidelity,
    Ambience,
    Surround,
    DynamicBoost,
    Bass,
}

impl FxEffect {
    /// Every effect, in chain order. Adding a variant forces this array to be
    /// updated, propagating to every `ALL` iteration.
    pub const ALL: [FxEffect; 5] = [
        FxEffect::Fidelity,
        FxEffect::Ambience,
        FxEffect::Surround,
        FxEffect::DynamicBoost,
        FxEffect::Bass,
    ];
}

#[derive(Debug)]
pub struct ProcessorChain {
    pub channels: usize,
    pub sample_rate: f64,
    pub enabled: bool,
    pub preamp_db: f64,
    pub filters: Vec<ApoFilter>,
    pub fidelity: FidelityEffect,
    pub ambience: AmbienceEffect,
    pub surround: SurroundEffect,
    pub dynamic_boost: DynamicBoostEffect,
    pub bass: BassBoostEffect,
}

impl ProcessorChain {
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
            buf.iter_mut().for_each(|s| *s *= gain);
        }

        for filter in &mut self.filters {
            let frames = buf.len() / channels;
            for frame in 0..frames {
                for ch in 0..channels {
                    buf[frame * channels + ch] =
                        filter.process_channel(buf[frame * channels + ch], ch);
                }
            }
        }

        self.fidelity.process(buf, channels);
        self.ambience.process(buf, channels);
        self.surround.process(buf, channels);
        self.dynamic_boost.process(buf, channels);
        self.bass.process(buf, channels);
    }

    pub fn set_effect_intensity(&mut self, effect: FxEffect, value: f64) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_intensity(value),
            FxEffect::Ambience => self.ambience.set_intensity(value),
            FxEffect::Surround => self.surround.set_intensity(value),
            FxEffect::DynamicBoost => self.dynamic_boost.set_intensity(value),
            FxEffect::Bass => self.bass.set_intensity(value),
        }
    }

    /// `(intensity, enabled)` for one effect — the read counterpart of
    /// `set_effect_intensity` / `set_effect_enabled`, so callers can iterate
    /// `FxEffect::ALL` instead of unrolling all five effects by hand.
    pub fn effect_params(&self, effect: FxEffect) -> (f64, bool) {
        match effect {
            FxEffect::Fidelity => (self.fidelity.intensity(), self.fidelity.enabled()),
            FxEffect::Ambience => (self.ambience.intensity(), self.ambience.enabled()),
            FxEffect::Surround => (self.surround.intensity(), self.surround.enabled()),
            FxEffect::DynamicBoost => {
                (self.dynamic_boost.intensity(), self.dynamic_boost.enabled())
            }
            FxEffect::Bass => (self.bass.intensity(), self.bass.enabled()),
        }
    }

    pub fn set_effect_enabled(&mut self, effect: FxEffect, on: bool) {
        match effect {
            FxEffect::Fidelity => self.fidelity.set_enabled(on),
            FxEffect::Ambience => self.ambience.set_enabled(on),
            FxEffect::Surround => self.surround.set_enabled(on),
            FxEffect::DynamicBoost => self.dynamic_boost.set_enabled(on),
            FxEffect::Bass => self.bass.set_enabled(on),
        }
    }

    pub fn reset(&mut self) {
        self.filters.iter_mut().for_each(|f| f.reset());
        self.fidelity.reset();
        self.ambience.reset();
        self.surround.reset();
        self.dynamic_boost.reset();
        self.bass.reset();
    }

    /// Rebind every sample-rate-dependent coefficient to a new output rate.
    ///
    /// A device/format change (e.g. switching outputs) renegotiates the rate,
    /// which invalidates not just the biquad filters but the effects too (their
    /// internal filters and reverb delays are rate-derived). Filters are updated
    /// in place; effects are rebuilt at the new rate, carrying over intensity +
    /// enabled (their sample history resets, which is unavoidable on a rate
    /// change). No-op when the rate is unchanged.
    pub fn rebind_sample_rate(&mut self, sample_rate: f64) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        for f in self.filters.iter_mut() {
            // A band whose freq is at/above the new Nyquist (e.g. a 20 kHz band
            // after 48k→32k) can't be realized — `rebind` holds it inert rather
            // than leaving the old-rate coefficients live, and re-arms it on its
            // own if a later rate makes it realizable again. User `enabled`
            // intent is untouched.
            f.rebind(sample_rate);
        }
        let ch = self.channels;
        self.fidelity = carry_settings(&self.fidelity, FidelityEffect::new(ch, sample_rate));
        self.ambience = carry_settings(&self.ambience, AmbienceEffect::new(ch, sample_rate));
        self.surround = carry_settings(&self.surround, SurroundEffect::new(sample_rate));
        self.dynamic_boost =
            carry_settings(&self.dynamic_boost, DynamicBoostEffect::new(sample_rate));
        self.bass = carry_settings(&self.bass, BassBoostEffect::new(ch, sample_rate));
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
    pub fn channels(mut self, n: usize) -> Self {
        self.channels = n;
        self
    }

    pub fn sample_rate(mut self, sr: f64) -> Self {
        self.sample_rate = sr;
        self
    }

    pub fn preamp_db(mut self, db: f64) -> Self {
        self.preamp_db = db;
        self
    }

    pub fn add_filter(mut self, filter: ApoFilter) -> Self {
        self.filters.push(filter);
        self
    }

    pub fn build(self) -> ProcessorChain {
        let channels = self.channels;
        let sr = self.sample_rate;
        ProcessorChain {
            channels,
            sample_rate: sr,
            enabled: true,
            preamp_db: self.preamp_db,
            filters: self.filters,
            fidelity: FidelityEffect::new(channels, sr),
            ambience: AmbienceEffect::new(channels, sr),
            surround: SurroundEffect::new(sr),
            dynamic_boost: DynamicBoostEffect::new(sr),
            bass: BassBoostEffect::new(channels, sr),
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
    fn full_default_chain_is_bit_perfect_passthrough() {
        // Default chain: no filters, all effects at 0 intensity, preamp 0.
        // Must pass audio through bit-for-bit.
        let mut chain = ProcessorChain::builder().build();
        let input: Vec<f64> = (0..256).map(|i| ((i as f64) * 0.013).sin() * 0.7).collect();
        let mut buf = input.clone();
        chain.process(&mut buf);
        assert_eq!(buf, input);
    }
}
