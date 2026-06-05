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
