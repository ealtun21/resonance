use resonance_dsp::{
    chain::{FxEffect, ProcessorChain, ProcessorChainBuilder},
    filter::{ApoFilter, FilterType},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub preamp_db: f64,
    pub eq_enabled: bool,
    pub bands: Vec<EqBand>,
    pub effects: FxEffects,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqBand {
    pub filter_type: ApoFilterType,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum ApoFilterType {
    Peaking,
    LowShelf,
    LowShelf12Db,
    LowShelfQ,
    HighShelf,
    HighShelf12Db,
    HighShelfQ,
    LowPass,
    LowPassQ,
    HighPass,
    HighPassQ,
    BandPass,
    Notch,
    AllPass,
}

impl From<ApoFilterType> for FilterType {
    fn from(t: ApoFilterType) -> Self {
        match t {
            ApoFilterType::Peaking => FilterType::Peaking,
            ApoFilterType::LowShelf => FilterType::LowShelf,
            ApoFilterType::LowShelf12Db => FilterType::LowShelf12Db,
            ApoFilterType::LowShelfQ => FilterType::LowShelfQ,
            ApoFilterType::HighShelf => FilterType::HighShelf,
            ApoFilterType::HighShelf12Db => FilterType::HighShelf12Db,
            ApoFilterType::HighShelfQ => FilterType::HighShelfQ,
            ApoFilterType::LowPass => FilterType::LowPass,
            ApoFilterType::LowPassQ => FilterType::LowPassQ,
            ApoFilterType::HighPass => FilterType::HighPass,
            ApoFilterType::HighPassQ => FilterType::HighPassQ,
            ApoFilterType::BandPass => FilterType::BandPass,
            ApoFilterType::Notch => FilterType::Notch,
            ApoFilterType::AllPass => FilterType::AllPass,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FxEffects {
    pub fidelity: EffectState,
    pub ambience: EffectState,
    pub surround: EffectState,
    pub dynamic_boost: EffectState,
    pub bass: EffectState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectState {
    pub enabled: bool,
    /// 0.0–1.0 normalized intensity
    pub intensity: f64,
}

impl Default for EffectState {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 0.0,
        }
    }
}

impl Preset {
    pub fn into_chain(self, channels: usize, sample_rate: f64) -> ProcessorChain {
        let mut builder = ProcessorChainBuilder::default()
            .channels(channels)
            .sample_rate(sample_rate)
            .preamp_db(self.preamp_db);

        if self.eq_enabled {
            for band in &self.bands {
                if let Ok(filter) = ApoFilter::builder()
                    .filter_type(band.filter_type.into())
                    .freq(band.freq)
                    .gain_db(band.gain_db)
                    .q(band.q)
                    .enabled(band.enabled)
                    .channels(channels)
                    .sample_rate(sample_rate)
                    .build()
                {
                    builder = builder.add_filter(filter);
                }
            }
        }

        let mut chain = builder.build();

        let apply = |chain: &mut ProcessorChain, effect: FxEffect, state: &EffectState| {
            chain.set_effect_intensity(effect, state.intensity);
            chain.set_effect_enabled(effect, state.enabled);
        };

        apply(&mut chain, FxEffect::Fidelity, &self.effects.fidelity);
        apply(&mut chain, FxEffect::Ambience, &self.effects.ambience);
        apply(&mut chain, FxEffect::Surround, &self.effects.surround);
        apply(
            &mut chain,
            FxEffect::DynamicBoost,
            &self.effects.dynamic_boost,
        );
        apply(&mut chain, FxEffect::Bass, &self.effects.bass);

        chain
    }
}
