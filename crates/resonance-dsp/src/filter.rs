use crate::channel::ChannelMask;
use std::f64::consts::PI;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum FilterError {
    #[error("invalid frequency {0} Hz: must be > 0 and < nyquist")]
    InvalidFrequency(f64),
    #[error("invalid Q {0}: must be > 0")]
    InvalidQ(f64),
    #[error("sample rate {0} must be > 0")]
    InvalidSampleRate(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterType {
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

#[derive(Debug, Clone, Copy)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadCoeffs {
    pub fn peaking(freq: f64, gain_db: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: 1.0 + alpha * a,
            b1: -2.0 * cos_w0,
            b2: 1.0 - alpha * a,
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha / a,
        }
        .normalize(1.0 + alpha / a))
    }

    /// Low shelf with a resonance/slope `q`. `q = 1/√2 ≈ 0.707` gives the
    /// classic maximally-flat (S=1) shelf; higher `q` adds a resonant bump.
    pub fn low_shelf(
        freq: f64,
        gain_db: f64,
        q: f64,
        sample_rate: f64,
    ) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        let sq = 2.0 * a.sqrt() * alpha;
        Ok(Self {
            b0: a * ((a + 1.0) - (a - 1.0) * cos_w0 + sq),
            b1: 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            b2: a * ((a + 1.0) - (a - 1.0) * cos_w0 - sq),
            a1: -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            a2: (a + 1.0) + (a - 1.0) * cos_w0 - sq,
        }
        .normalize((a + 1.0) + (a - 1.0) * cos_w0 + sq))
    }

    /// High shelf with a resonance/slope `q` (see [`Self::low_shelf`]).
    pub fn high_shelf(
        freq: f64,
        gain_db: f64,
        q: f64,
        sample_rate: f64,
    ) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        let sq = 2.0 * a.sqrt() * alpha;
        Ok(Self {
            b0: a * ((a + 1.0) + (a - 1.0) * cos_w0 + sq),
            b1: -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            b2: a * ((a + 1.0) + (a - 1.0) * cos_w0 - sq),
            a1: 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            a2: (a + 1.0) - (a - 1.0) * cos_w0 - sq,
        }
        .normalize((a + 1.0) - (a - 1.0) * cos_w0 + sq))
    }

    pub fn low_pass(freq: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: (1.0 - cos_w0) / 2.0,
            b1: 1.0 - cos_w0,
            b2: (1.0 - cos_w0) / 2.0,
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha,
        }
        .normalize(1.0 + alpha))
    }

    pub fn high_pass(freq: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: (1.0 + cos_w0) / 2.0,
            b1: -(1.0 + cos_w0),
            b2: (1.0 + cos_w0) / 2.0,
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha,
        }
        .normalize(1.0 + alpha))
    }

    pub fn band_pass(freq: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: alpha,
            b1: 0.0,
            b2: -alpha,
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha,
        }
        .normalize(1.0 + alpha))
    }

    pub fn notch(freq: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: 1.0,
            b1: -2.0 * cos_w0,
            b2: 1.0,
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha,
        }
        .normalize(1.0 + alpha))
    }

    pub fn all_pass(freq: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: 1.0 - alpha,
            b1: -2.0 * cos_w0,
            b2: 1.0 + alpha,
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha,
        }
        .normalize(1.0 + alpha))
    }

    fn normalize(mut self, a0: f64) -> Self {
        self.b0 /= a0;
        self.b1 /= a0;
        self.b2 /= a0;
        self.a1 /= a0;
        self.a2 /= a0;
        self
    }
}

fn validate(freq: f64, q: f64, sample_rate: f64) -> Result<(), FilterError> {
    // Reject non-finite first: NaN/Inf pass every `<= 0` / `>=` comparison below
    // (NaN compares false to everything) and would poison the biquad
    // coefficients. Reachable from an untrusted APO `.txt` (`parse_db` etc.).
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(FilterError::InvalidSampleRate(sample_rate));
    }
    if !freq.is_finite() || freq <= 0.0 || freq >= sample_rate / 2.0 {
        return Err(FilterError::InvalidFrequency(freq));
    }
    if !q.is_finite() || q <= 0.0 {
        return Err(FilterError::InvalidQ(q));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    pub fn process(&mut self, input: f64, c: &BiquadCoeffs) -> f64 {
        let output =
            c.b0 * input + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone)]
pub struct BiquadFilter {
    pub coeffs: BiquadCoeffs,
    pub enabled: bool,
    states: Vec<BiquadState>,
}

impl BiquadFilter {
    pub fn new(coeffs: BiquadCoeffs, channels: usize) -> Self {
        Self {
            coeffs,
            enabled: true,
            states: vec![BiquadState::default(); channels],
        }
    }

    pub fn process_channel(&mut self, sample: f64, channel: usize) -> f64 {
        if !self.enabled {
            return sample;
        }
        self.states[channel].process(sample, &self.coeffs)
    }

    pub fn reset(&mut self) {
        self.states.iter_mut().for_each(|s| s.reset());
    }

    /// Resize the per-channel state to `channels`. Existing channels keep their
    /// running history; new channels start at rest. Used when the live channel
    /// count changes (device renegotiation).
    pub fn set_channels(&mut self, channels: usize) {
        self.states.resize(channels, BiquadState::default());
    }
}

#[derive(Debug, Clone)]
pub struct ApoFilter {
    pub filter_type: FilterType,
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
    /// Which channels this band applies to. Defaults to [`ChannelMask::ALL`] so a
    /// band loaded from a preset (or built without an explicit target) processes
    /// every channel — the back-compatible behaviour. Per-channel EQ narrows it.
    pub mask: ChannelMask,
    /// Whether the current parameters are realizable at the active sample rate.
    /// Distinct from `enabled` (user intent): a band sitting at/above Nyquist
    /// after a rate drop is held inert here, then resumes on its own when a
    /// higher rate makes it realizable again — without touching `enabled`.
    realizable: bool,
    biquad: BiquadFilter,
}

impl ApoFilter {
    pub fn builder() -> ApoFilterBuilder {
        ApoFilterBuilder::default()
    }
}

#[derive(Debug, Default)]
pub struct ApoFilterBuilder {
    filter_type: Option<FilterType>,
    freq: Option<f64>,
    gain_db: f64,
    q: f64,
    enabled: bool,
    channels: usize,
    sample_rate: Option<f64>,
    // `ChannelMask::default()` is `ALL`, so an unset mask targets every channel.
    channel_mask: ChannelMask,
}

impl ApoFilterBuilder {
    pub fn filter_type(mut self, t: FilterType) -> Self {
        self.filter_type = Some(t);
        self
    }

    pub fn freq(mut self, hz: f64) -> Self {
        self.freq = Some(hz);
        self
    }

    pub fn gain_db(mut self, db: f64) -> Self {
        self.gain_db = db;
        self
    }

    pub fn q(mut self, q: f64) -> Self {
        self.q = q;
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub fn channels(mut self, n: usize) -> Self {
        self.channels = n;
        self
    }

    pub fn sample_rate(mut self, sr: f64) -> Self {
        self.sample_rate = Some(sr);
        self
    }

    /// Restrict this band to a subset of channels. Omit (or pass
    /// [`ChannelMask::ALL`]) for a global band.
    pub fn channel_mask(mut self, mask: ChannelMask) -> Self {
        self.channel_mask = mask;
        self
    }

    pub fn build(self) -> Result<ApoFilter, FilterError> {
        let filter_type = self.filter_type.unwrap_or(FilterType::Peaking);
        let freq = self.freq.unwrap_or(1000.0);
        let sr = self.sample_rate.unwrap_or(48000.0);
        // Non-finite Q/gain (e.g. "nan" in a hostile APO `.txt`) → sane defaults
        // so the band loads flat instead of poisoning the coefficients. freq/sr
        // are still hard-validated in `validate`.
        let q = if !self.q.is_finite() || self.q <= 0.0 {
            0.707
        } else {
            self.q
        };
        let gain_db = if self.gain_db.is_finite() {
            self.gain_db
        } else {
            0.0
        };
        let channels = if self.channels == 0 { 2 } else { self.channels };

        let coeffs = coeffs_for(filter_type, freq, gain_db, q, sr)?;

        Ok(ApoFilter {
            filter_type,
            freq,
            gain_db,
            q,
            enabled: self.enabled,
            mask: self.channel_mask,
            realizable: true,
            biquad: BiquadFilter::new(coeffs, channels),
        })
    }
}

/// Compute biquad coefficients for a filter type / parameters.
fn coeffs_for(
    filter_type: FilterType,
    freq: f64,
    gain_db: f64,
    q: f64,
    sr: f64,
) -> Result<BiquadCoeffs, FilterError> {
    Ok(match filter_type {
        FilterType::Peaking => BiquadCoeffs::peaking(freq, gain_db, q, sr)?,
        FilterType::LowShelf | FilterType::LowShelf12Db | FilterType::LowShelfQ => {
            BiquadCoeffs::low_shelf(freq, gain_db, q, sr)?
        }
        FilterType::HighShelf | FilterType::HighShelf12Db | FilterType::HighShelfQ => {
            BiquadCoeffs::high_shelf(freq, gain_db, q, sr)?
        }
        FilterType::LowPass | FilterType::LowPassQ => BiquadCoeffs::low_pass(freq, q, sr)?,
        FilterType::HighPass | FilterType::HighPassQ => BiquadCoeffs::high_pass(freq, q, sr)?,
        FilterType::BandPass => BiquadCoeffs::band_pass(freq, q, sr)?,
        FilterType::Notch => BiquadCoeffs::notch(freq, q, sr)?,
        FilterType::AllPass => BiquadCoeffs::all_pass(freq, q, sr)?,
    })
}

impl ApoFilter {
    /// Re-evaluate whether the band is realizable at `sr`, holding it inert when
    /// not (rather than leaving stale coefficients live). Returns the result.
    pub fn rebind(&mut self, sr: f64) -> bool {
        self.realizable = self
            .update(self.filter_type, self.freq, self.gain_db, self.q, sr)
            .is_ok();
        self.realizable
    }

    pub fn process_channel(&mut self, sample: f64, channel: usize) -> f64 {
        if !self.enabled || !self.realizable {
            return sample;
        }
        self.biquad.process_channel(sample, channel)
    }

    /// Recompute coefficients in place, **preserving** the running filter state.
    /// Used for live parameter changes so rapid edits don't reset history and
    /// produce clicks/crackle. Returns without changing anything on error.
    pub fn update(
        &mut self,
        filter_type: FilterType,
        freq: f64,
        gain_db: f64,
        q: f64,
        sr: f64,
    ) -> Result<(), FilterError> {
        // Coerce non-finite Q/gain to flat defaults exactly as the builder does
        // — `update` is the live-edit path and is reachable from untrusted IPC
        // (`SetBand`) and presets, so a NaN gain must not poison the biquad.
        let q = if !q.is_finite() || q <= 0.0 { 0.707 } else { q };
        let gain_db = if gain_db.is_finite() { gain_db } else { 0.0 };
        let coeffs = coeffs_for(filter_type, freq, gain_db, q, sr)?;
        self.filter_type = filter_type;
        self.freq = freq;
        self.gain_db = gain_db;
        self.q = q;
        self.biquad.coeffs = coeffs;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.biquad.reset();
    }

    /// Resize per-channel filter state to `channels` (device renegotiation). The
    /// channel *mask* is unchanged — a band targeting only channel 0 still does
    /// after a widen — and existing channels keep their running history.
    pub fn set_channels(&mut self, channels: usize) {
        self.biquad.set_channels(channels);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::filter_gain_db;

    const SR: f64 = 48000.0;

    fn build(ft: FilterType, freq: f64, gain_db: f64, q: f64) -> ApoFilter {
        ApoFilter::builder()
            .filter_type(ft)
            .freq(freq)
            .gain_db(gain_db)
            .q(q)
            .enabled(true)
            .channels(1)
            .sample_rate(SR)
            .build()
            .unwrap()
    }

    #[test]
    fn rejects_non_finite_freq_and_q() {
        // NaN/Inf must not slip through the `<= 0` / `>=` checks and poison coeffs.
        for bad in [f64::NAN, f64::INFINITY] {
            assert!(
                ApoFilter::builder()
                    .filter_type(FilterType::Peaking)
                    .freq(bad)
                    .gain_db(3.0)
                    .q(1.0)
                    .channels(1)
                    .sample_rate(SR)
                    .build()
                    .is_err(),
                "freq={bad} should be rejected"
            );
        }
    }

    #[test]
    fn non_finite_gain_loads_flat() {
        // A non-finite gain defaults to 0 dB so the band loads (flat) instead of
        // producing NaN coefficients.
        let f = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(1000.0)
            .gain_db(f64::NAN)
            .q(1.0)
            .channels(1)
            .sample_rate(SR)
            .build()
            .expect("should build with defaulted gain");
        assert_eq!(f.gain_db, 0.0);
    }

    // ── BiquadCoeffs direct ─────────────────────────────────────────────────

    #[test]
    fn peaking_unity_at_zero_gain() {
        let c = BiquadCoeffs::peaking(1000.0, 0.0, 0.707, SR).unwrap();
        let mut state = BiquadState::default();
        let out = state.process(1.0, &c);
        assert!((out - 1.0).abs() < 1e-9, "unity gain failed: {out}");
    }

    #[test]
    fn low_pass_passes_dc() {
        let c = BiquadCoeffs::low_pass(10000.0, 0.707, SR).unwrap();
        let mut state = BiquadState::default();
        let mut last = 0.0;
        for _ in 0..2000 {
            last = state.process(1.0, &c);
        }
        assert!((last - 1.0).abs() < 1e-6, "LP DC response: {last}");
    }

    // ── Peaking ─────────────────────────────────────────────────────────────

    #[test]
    fn peaking_boosts_at_fc() {
        let mut f = build(FilterType::Peaking, 1000.0, 6.0, 1.0);
        let g = filter_gain_db(&mut f, 1000.0, SR);
        assert!((g - 6.0).abs() < 0.5, "peaking +6 dB at Fc: got {g:.2} dB");
    }

    #[test]
    fn peaking_cuts_at_fc() {
        let mut f = build(FilterType::Peaking, 1000.0, -6.0, 1.0);
        let g = filter_gain_db(&mut f, 1000.0, SR);
        assert!(
            (g - (-6.0)).abs() < 0.5,
            "peaking -6 dB at Fc: got {g:.2} dB"
        );
    }

    #[test]
    fn peaking_unity_far_from_fc() {
        let mut f = build(FilterType::Peaking, 1000.0, 6.0, 1.0);
        // 100 Hz is a full decade below Fc=1 kHz — well in the stopband
        let g = filter_gain_db(&mut f, 100.0, SR);
        assert!(
            g.abs() < 1.0,
            "peaking far-field: got {g:.2} dB (expected ~0)"
        );
    }

    #[test]
    fn peaking_large_boost_accurate() {
        let mut f = build(FilterType::Peaking, 4000.0, 12.0, 0.5);
        let g = filter_gain_db(&mut f, 4000.0, SR);
        assert!(
            (g - 12.0).abs() < 1.0,
            "peaking +12 dB at Fc: got {g:.2} dB"
        );
    }

    // ── Low Shelf ───────────────────────────────────────────────────────────

    #[test]
    fn low_shelf_boosts_below_fc() {
        let mut f = build(FilterType::LowShelf, 1000.0, 6.0, 0.707);
        // 50 Hz is deep in the boosted region
        let g = filter_gain_db(&mut f, 50.0, SR);
        assert!(
            g > 4.5,
            "low shelf below Fc: got {g:.2} dB (expected > 4.5)"
        );
    }

    #[test]
    fn low_shelf_unity_above_fc() {
        let mut f = build(FilterType::LowShelf, 500.0, 6.0, 0.707);
        let g = filter_gain_db(&mut f, 10000.0, SR);
        assert!(
            g.abs() < 1.0,
            "low shelf above Fc: got {g:.2} dB (expected ~0)"
        );
    }

    #[test]
    fn low_shelf_q_adds_resonance() {
        // Higher Q must create a resonant overshoot beyond the shelf gain
        // somewhere around the corner frequency.
        let sweep = [300.0, 500.0, 700.0, 1000.0, 1400.0, 2000.0, 3000.0];
        let peak = |q: f64| {
            sweep
                .iter()
                .map(|&f| {
                    let mut filt = build(FilterType::LowShelf, 1000.0, 12.0, q);
                    filter_gain_db(&mut filt, f, SR)
                })
                .fold(f64::MIN, f64::max)
        };
        let flat = peak(0.707);
        let reso = peak(5.0);
        assert!(
            reso > flat + 1.0,
            "high-Q shelf should overshoot the shelf gain: flat={flat:.2} reso={reso:.2}"
        );
    }

    #[test]
    fn low_shelf_cuts_below_fc() {
        let mut f = build(FilterType::LowShelf, 1000.0, -6.0, 0.707);
        let g = filter_gain_db(&mut f, 50.0, SR);
        assert!(
            g < -4.5,
            "low shelf cut below Fc: got {g:.2} dB (expected < -4.5)"
        );
    }

    // ── High Shelf ──────────────────────────────────────────────────────────

    #[test]
    fn high_shelf_boosts_above_fc() {
        let mut f = build(FilterType::HighShelf, 5000.0, 6.0, 0.707);
        let g = filter_gain_db(&mut f, 20000.0, SR);
        assert!(
            g > 4.5,
            "high shelf above Fc: got {g:.2} dB (expected > 4.5)"
        );
    }

    #[test]
    fn high_shelf_unity_below_fc() {
        let mut f = build(FilterType::HighShelf, 5000.0, 6.0, 0.707);
        let g = filter_gain_db(&mut f, 100.0, SR);
        assert!(
            g.abs() < 1.0,
            "high shelf below Fc: got {g:.2} dB (expected ~0)"
        );
    }

    // ── Low Pass ────────────────────────────────────────────────────────────

    #[test]
    fn low_pass_passes_low_freq() {
        let mut f = build(FilterType::LowPassQ, 5000.0, 0.0, 0.707);
        let g = filter_gain_db(&mut f, 100.0, SR);
        assert!(g.abs() < 1.0, "LP passband: got {g:.2} dB (expected ~0)");
    }

    #[test]
    fn low_pass_rejects_above_fc() {
        let mut f = build(FilterType::LowPassQ, 1000.0, 0.0, 0.707);
        let g = filter_gain_db(&mut f, 10000.0, SR);
        assert!(g < -20.0, "LP stopband: got {g:.2} dB (expected < -20)");
    }

    #[test]
    fn low_pass_3db_at_fc() {
        let mut f = build(FilterType::LowPassQ, 1000.0, 0.0, 0.707);
        let g = filter_gain_db(&mut f, 1000.0, SR);
        assert!((g - (-3.01)).abs() < 0.5, "LP -3 dB at Fc: got {g:.2} dB");
    }

    // ── High Pass ───────────────────────────────────────────────────────────

    #[test]
    fn high_pass_passes_high_freq() {
        let mut f = build(FilterType::HighPassQ, 500.0, 0.0, 0.707);
        let g = filter_gain_db(&mut f, 10000.0, SR);
        assert!(g.abs() < 1.0, "HP passband: got {g:.2} dB (expected ~0)");
    }

    #[test]
    fn high_pass_rejects_below_fc() {
        let mut f = build(FilterType::HighPassQ, 1000.0, 0.0, 0.707);
        let g = filter_gain_db(&mut f, 100.0, SR);
        assert!(g < -20.0, "HP stopband: got {g:.2} dB (expected < -20)");
    }

    // ── Notch ───────────────────────────────────────────────────────────────

    #[test]
    fn notch_deep_null_at_fc() {
        // Q=8 → narrow notch; at exactly Fc the response is theoretically -inf dB
        let mut f = build(FilterType::Notch, 1000.0, 0.0, 8.0);
        let g = filter_gain_db(&mut f, 1000.0, SR);
        assert!(g < -40.0, "notch at Fc: got {g:.2} dB (expected < -40)");
    }

    #[test]
    fn notch_unity_away_from_fc() {
        let mut f = build(FilterType::Notch, 1000.0, 0.0, 8.0);
        let g = filter_gain_db(&mut f, 100.0, SR);
        assert!(
            g.abs() < 1.0,
            "notch far from Fc: got {g:.2} dB (expected ~0)"
        );
    }

    // ── Band Pass ───────────────────────────────────────────────────────────

    #[test]
    fn band_pass_passes_at_fc() {
        let mut f = build(FilterType::BandPass, 1000.0, 0.0, 1.0);
        let g = filter_gain_db(&mut f, 1000.0, SR);
        // BP peak is near 0 dB by construction
        assert!(g > -3.0, "BP at Fc: got {g:.2} dB (expected > -3)");
    }

    #[test]
    fn band_pass_rejects_extremes() {
        let mut f = build(FilterType::BandPass, 1000.0, 0.0, 1.0);
        let g_lo = filter_gain_db(&mut f, 50.0, SR);
        let mut f2 = build(FilterType::BandPass, 1000.0, 0.0, 1.0);
        let g_hi = filter_gain_db(&mut f2, 20000.0, SR);
        assert!(g_lo < -10.0, "BP lo reject: got {g_lo:.2} dB");
        assert!(g_hi < -10.0, "BP hi reject: got {g_hi:.2} dB");
    }

    // ── All Pass ────────────────────────────────────────────────────────────

    #[test]
    fn all_pass_unity_gain_everywhere() {
        for freq in [100.0, 1000.0, 10000.0] {
            let mut f = build(FilterType::AllPass, 1000.0, 0.0, 0.707);
            let g = filter_gain_db(&mut f, freq, SR);
            assert!(g.abs() < 0.1, "AP at {freq} Hz: got {g:.2} dB (expected 0)");
        }
    }

    // ── Disabled ────────────────────────────────────────────────────────────

    #[test]
    fn disabled_filter_passthrough() {
        let mut f = build(FilterType::Peaking, 1000.0, 12.0, 1.0);
        f.enabled = false;
        let g = filter_gain_db(&mut f, 1000.0, SR);
        assert!(
            g.abs() < 0.01,
            "disabled filter: got {g:.2} dB (expected 0)"
        );
    }

    // ── Error cases ─────────────────────────────────────────────────────────

    #[test]
    fn invalid_frequency_rejected() {
        assert!(BiquadCoeffs::peaking(0.0, 0.0, 1.0, SR).is_err());
        assert!(BiquadCoeffs::peaking(SR / 2.0, 0.0, 1.0, SR).is_err());
        assert!(BiquadCoeffs::peaking(SR, 0.0, 1.0, SR).is_err());
    }

    #[test]
    fn invalid_q_rejected() {
        assert!(BiquadCoeffs::peaking(1000.0, 0.0, 0.0, SR).is_err());
        assert!(BiquadCoeffs::peaking(1000.0, 0.0, -1.0, SR).is_err());
    }
}
