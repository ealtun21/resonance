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

/// Stereo scope of an EQ band. `Stereo` (the default) processes each channel
/// independently; `Mid`/`Side` process the mono sum / stereo difference of the
/// front L/R pair, leaving any further channels untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BandScope {
    #[default]
    Stereo,
    Mid,
    Side,
}

/// Per-band dynamic EQ parameters: the band's gain morphs from `gain_db`
/// toward `gain_db + range_db` as the in-band level rises past `threshold_db`
/// (feed-forward sidechain, zero added latency). See `ApoFilter::set_dynamics`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynParams {
    /// Detector level (dBFS) where the morph starts.
    pub threshold_db: f64,
    /// Signed max gain offset: negative = cut when loud (de-ess), positive =
    /// boost when loud.
    pub range_db: f64,
    /// Detector attack time constant (ms).
    pub attack_ms: f64,
    /// Detector release time constant (ms).
    pub release_ms: f64,
}

impl DynParams {
    pub const DEFAULT: Self = Self {
        threshold_db: -30.0,
        range_db: -6.0,
        attack_ms: 5.0,
        release_ms: 150.0,
    };

    /// Clamp every field into its supported range; non-finite values (hostile
    /// IPC/profile input) fall back to that field's default — same posture as
    /// the Q/gain coercion in `ApoFilter::update`.
    #[must_use]
    pub fn clamped(self) -> Self {
        let sane = |v: f64, lo: f64, hi: f64, default: f64| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        Self {
            threshold_db: sane(self.threshold_db, -80.0, 0.0, Self::DEFAULT.threshold_db),
            range_db: sane(self.range_db, -24.0, 24.0, Self::DEFAULT.range_db),
            attack_ms: sane(self.attack_ms, 0.1, 500.0, Self::DEFAULT.attack_ms),
            release_ms: sane(self.release_ms, 1.0, 5000.0, Self::DEFAULT.release_ms),
        }
    }
}

/// Runtime state of a band's dynamics: linked band-pass sidechain → one-pole
/// peak envelope → gain-offset morph of the head peaking biquad.
#[derive(Debug, Clone)]
struct DynState {
    params: DynParams,
    /// Band-pass at the band's Fc/Q — only in-band energy triggers the morph.
    sc_coeffs: BiquadCoeffs,
    /// ONE detector state (linked): every channel gets the same offset so the
    /// stereo image never wobbles.
    sc_state: BiquadState,
    /// Linear peak envelope of the rectified sidechain.
    env: f64,
    /// One-pole smoothing coefficients derived from attack/release + rate.
    att: f64,
    rel: f64,
    /// Cached trig of the head peaking biquad — its ω-terms don't depend on
    /// gain, so a morph only recomputes the gain factor.
    cos_w0: f64,
    alpha: f64,
    /// Currently applied gain offset (dB).
    offset_db: f64,
}

fn make_dyn_state(params: DynParams, freq: f64, q: f64, sr: f64) -> Result<DynState, FilterError> {
    let params = params.clamped();
    let sc_coeffs = BiquadCoeffs::band_pass(freq, q, sr)?;
    let w0 = 2.0 * PI * freq / sr;
    Ok(DynState {
        params,
        sc_coeffs,
        sc_state: BiquadState::default(),
        env: 0.0,
        att: 1.0 - (-1000.0 / (params.attack_ms * sr)).exp(),
        rel: 1.0 - (-1000.0 / (params.release_ms * sr)).exp(),
        cos_w0: w0.cos(),
        alpha: w0.sin() / (2.0 * q),
        offset_db: 0.0,
    })
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
    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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
    ///
    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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
    ///
    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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

    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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

    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
    pub fn high_pass(freq: f64, q: f64, sample_rate: f64) -> Result<Self, FilterError> {
        validate(freq, q, sample_rate)?;
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        Ok(Self {
            b0: f64::midpoint(1.0, cos_w0),
            b1: -(1.0 + cos_w0),
            b2: f64::midpoint(1.0, cos_w0),
            a1: -2.0 * cos_w0,
            a2: 1.0 - alpha,
        }
        .normalize(1.0 + alpha))
    }

    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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

    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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

    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sample_rate` are non-finite or
    /// out of range (`freq` must be in `(0, sample_rate/2)`, `q > 0`,
    /// `sample_rate > 0`).
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
    #[must_use]
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
        self.states.iter_mut().for_each(BiquadState::reset);
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
    /// Filter slope in dB/oct (12/24/48) for shelves + HP/LP; ignored by the
    /// single-biquad types. 12 = the original single-biquad behaviour.
    pub slope_db_oct: u8,
    /// Stereo scope: `Stereo` (per-channel, the default) or `Mid`/`Side`
    /// (process the mono sum / stereo difference of the front L/R pair).
    pub scope: BandScope,
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
    /// Extra cascaded sections for steeper slopes (empty at 12 dB/oct, so the
    /// common path is byte-identical to a single biquad).
    extra: Vec<BiquadFilter>,
    /// Dynamic EQ state (level-driven gain morph). Invariant: only ever
    /// `Some` on a `Peaking` band — the single-biquad type whose gain-only
    /// coefficient morph is cheap from cached trig.
    dyn_state: Option<DynState>,
}

impl ApoFilter {
    #[must_use]
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
    slope_db_oct: u8,
    scope: BandScope,
    enabled: bool,
    channels: usize,
    sample_rate: Option<f64>,
    // `ChannelMask::default()` is `ALL`, so an unset mask targets every channel.
    channel_mask: ChannelMask,
    dynamics: Option<DynParams>,
}

impl ApoFilterBuilder {
    #[must_use]
    pub fn filter_type(mut self, t: FilterType) -> Self {
        self.filter_type = Some(t);
        self
    }

    #[must_use]
    pub fn freq(mut self, hz: f64) -> Self {
        self.freq = Some(hz);
        self
    }

    #[must_use]
    pub fn gain_db(mut self, db: f64) -> Self {
        self.gain_db = db;
        self
    }

    #[must_use]
    pub fn q(mut self, q: f64) -> Self {
        self.q = q;
        self
    }

    /// Filter slope in dB/oct — 12 (default, single biquad), 24, or 48. Applies
    /// to shelves + HP/LP; ignored by the single-biquad types.
    #[must_use]
    pub fn slope_db_oct(mut self, slope: u8) -> Self {
        self.slope_db_oct = slope;
        self
    }

    /// Stereo scope of the band — `Stereo` (default), `Mid`, or `Side`.
    #[must_use]
    pub fn scope(mut self, scope: BandScope) -> Self {
        self.scope = scope;
        self
    }

    #[must_use]
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    #[must_use]
    pub fn channels(mut self, n: usize) -> Self {
        self.channels = n;
        self
    }

    #[must_use]
    pub fn sample_rate(mut self, sr: f64) -> Self {
        self.sample_rate = Some(sr);
        self
    }

    /// Restrict this band to a subset of channels. Omit (or pass
    /// [`ChannelMask::ALL`]) for a global band.
    #[must_use]
    pub fn channel_mask(mut self, mask: ChannelMask) -> Self {
        self.channel_mask = mask;
        self
    }

    /// Attach dynamic EQ parameters (level-driven gain morph). Only honoured
    /// on `Peaking` bands — silently ignored elsewhere (front-ends gate the
    /// control on [`super::filter::FilterType::Peaking`]).
    #[must_use]
    pub fn dynamics(mut self, params: Option<DynParams>) -> Self {
        self.dynamics = params;
        self
    }

    /// # Errors
    /// Returns [`FilterError`] if the resolved `freq`/`sample_rate` are non-finite
    /// or out of range (`freq` must be in `(0, sample_rate/2)`, `sample_rate > 0`).
    /// Non-finite `q`/`gain_db` are coerced to flat defaults rather than rejected.
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
        let slope_db_oct = normalize_slope(self.slope_db_oct);

        let mut coeffs = section_coeffs(filter_type, freq, gain_db, q, slope_db_oct, sr)?;
        let head = coeffs.remove(0);
        let extra = coeffs
            .into_iter()
            .map(|c| BiquadFilter::new(c, channels))
            .collect();
        let dyn_state = match self.dynamics {
            Some(p) if filter_type == FilterType::Peaking => Some(make_dyn_state(p, freq, q, sr)?),
            _ => None,
        };

        Ok(ApoFilter {
            filter_type,
            freq,
            gain_db,
            q,
            slope_db_oct,
            scope: self.scope,
            enabled: self.enabled,
            mask: self.channel_mask,
            realizable: true,
            biquad: BiquadFilter::new(head, channels),
            extra,
            dyn_state,
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

/// Per-section Butterworth Q values for an even-order low/high-pass cascade of
/// `sections` biquads. Their product is maximally flat with the −3 dB point
/// exactly at Fc for every order — the standard cascaded-biquad Butterworth
/// pole-Q table (2nd/4th/8th order).
fn butterworth_section_qs(sections: usize) -> &'static [f64] {
    match sections {
        2 => &[0.541_196_100, 1.306_562_965], // 4th order — 24 dB/oct
        4 => &[0.509_795_579, 0.601_344_887, 0.899_976_223, 2.562_915_447], // 8th order — 48 dB/oct
        _ => &[std::f64::consts::FRAC_1_SQRT_2], // 2nd order — 12 dB/oct
    }
}

/// Filter types whose slope is adjustable (shelves + HP/LP). Peaking, Notch,
/// `BandPass` and `AllPass` are single-biquad and ignore the slope.
fn is_slope_type(ft: FilterType) -> bool {
    matches!(
        ft,
        FilterType::LowShelf
            | FilterType::LowShelf12Db
            | FilterType::LowShelfQ
            | FilterType::HighShelf
            | FilterType::HighShelf12Db
            | FilterType::HighShelfQ
            | FilterType::LowPass
            | FilterType::LowPassQ
            | FilterType::HighPass
            | FilterType::HighPassQ
    )
}

/// Number of cascaded biquad sections for a slope: 12→1, 24→2, 48→4.
fn sections_for_slope(slope_db_oct: u8) -> usize {
    match slope_db_oct {
        24 => 2,
        48 => 4,
        _ => 1,
    }
}

/// Normalise an arbitrary slope value to the supported set {12, 24, 48} dB/oct.
fn normalize_slope(slope_db_oct: u8) -> u8 {
    match slope_db_oct {
        24 => 24,
        48 => 48,
        _ => 12,
    }
}

/// Coefficients for every cascaded section realising `filter_type` at `slope`.
/// A single biquad (identical to [`coeffs_for`]) for non-slope types or 12 dB/oct;
/// a Butterworth cascade otherwise. Shelves split their gain across sections so
/// the pass-band gain is unchanged as the slope steepens.
fn section_coeffs(
    filter_type: FilterType,
    freq: f64,
    gain_db: f64,
    q: f64,
    slope_db_oct: u8,
    sr: f64,
) -> Result<Vec<BiquadCoeffs>, FilterError> {
    let n = sections_for_slope(slope_db_oct);
    if n == 1 || !is_slope_type(filter_type) {
        return Ok(vec![coeffs_for(filter_type, freq, gain_db, q, sr)?]);
    }
    let per_section_gain = gain_db / n as f64;
    butterworth_section_qs(n)
        .iter()
        .map(|&qk| match filter_type {
            FilterType::LowPass | FilterType::LowPassQ => BiquadCoeffs::low_pass(freq, qk, sr),
            FilterType::HighPass | FilterType::HighPassQ => BiquadCoeffs::high_pass(freq, qk, sr),
            FilterType::LowShelf | FilterType::LowShelf12Db | FilterType::LowShelfQ => {
                BiquadCoeffs::low_shelf(freq, per_section_gain, qk, sr)
            }
            FilterType::HighShelf | FilterType::HighShelf12Db | FilterType::HighShelfQ => {
                BiquadCoeffs::high_shelf(freq, per_section_gain, qk, sr)
            }
            _ => unreachable!("non-slope types return a single biquad above"),
        })
        .collect()
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
        let mut y = self.biquad.process_channel(sample, channel);
        for section in &mut self.extra {
            y = section.process_channel(y, channel);
        }
        y
    }

    /// Recompute coefficients in place, **preserving** the running filter state.
    /// Used for live parameter changes so rapid edits don't reset history and
    /// produce clicks/crackle. Returns without changing anything on error.
    ///
    /// # Errors
    /// Returns [`FilterError`] if `freq`, `q`, or `sr` are non-finite or out of
    /// range (`freq` must be in `(0, sr/2)`, `q > 0`, `sr > 0`); the existing
    /// coefficients and state are left untouched in that case.
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
        let coeffs = section_coeffs(filter_type, freq, gain_db, q, self.slope_db_oct, sr)?;
        self.filter_type = filter_type;
        self.freq = freq;
        self.gain_db = gain_db;
        self.q = q;
        self.biquad.coeffs = coeffs[0];
        // Update the extra sections in place when the count is unchanged so live
        // parameter edits keep the running state (click-free); otherwise rebuild.
        let channels = self.biquad.states.len();
        if self.extra.len() == coeffs.len() - 1 {
            for (section, c) in self.extra.iter_mut().zip(&coeffs[1..]) {
                section.coeffs = *c;
            }
        } else {
            self.extra = coeffs[1..]
                .iter()
                .map(|c| BiquadFilter::new(*c, channels))
                .collect();
        }
        // Re-derive the dynamics detector at the new freq/q/rate. The head was
        // just set to the static response, so the applied offset restarts at 0
        // (the envelope carries over — a live param edit shouldn't retrigger
        // the attack). Dynamics are dropped if the type left Peaking.
        if let Some(d) = self.dyn_state.take() {
            if filter_type == FilterType::Peaking {
                let mut fresh = make_dyn_state(d.params, freq, q, sr)?;
                fresh.env = d.env;
                fresh.sc_state = d.sc_state;
                self.dyn_state = Some(fresh);
            }
        }
        Ok(())
    }

    /// Set (or clear) the band's dynamic EQ. `Some` is only honoured on
    /// `Peaking` bands — other types clear silently (front-ends gate the
    /// control). The head coefficients are restored to the static response;
    /// the morph re-engages from the incoming signal.
    ///
    /// # Errors
    /// Returns [`FilterError`] if the current parameters are not realisable at
    /// `sr` (same conditions as [`Self::update`]).
    pub fn set_dynamics(&mut self, params: Option<DynParams>, sr: f64) -> Result<(), FilterError> {
        self.dyn_state = match params {
            Some(p) if self.filter_type == FilterType::Peaking => {
                Some(make_dyn_state(p, self.freq, self.q, sr)?)
            }
            _ => None,
        };
        // Restore the static head coefficients (a previous morph may be live).
        self.update(self.filter_type, self.freq, self.gain_db, self.q, sr)
    }

    /// The band's dynamic EQ parameters (clamped), when set.
    #[must_use]
    pub fn dynamics(&self) -> Option<DynParams> {
        self.dyn_state.as_ref().map(|d| d.params)
    }

    /// Whether the dynamics morph is currently running (set + band active).
    /// The chain uses this to pick the frame-major detection path.
    #[must_use]
    pub fn dynamics_active(&self) -> bool {
        self.enabled && self.realizable && self.dyn_state.is_some()
    }

    /// Whether the band is realisable at the bound sample rate (see the
    /// `realizable` field docs — distinct from the user `enabled` intent).
    #[must_use]
    pub fn is_realizable(&self) -> bool {
        self.realizable
    }

    /// Every cascaded section's coefficients, head first — the band's full
    /// transfer function for response evaluation (FR display, linear-phase
    /// kernel synthesis).
    pub fn sections(&self) -> impl Iterator<Item = &BiquadCoeffs> {
        std::iter::once(&self.biquad.coeffs).chain(self.extra.iter().map(|s| &s.coeffs))
    }

    /// Per-frame dynamics hook: feed one detector sample (the band's input —
    /// masked-channel mean, or the Mid/Side value), update the envelope and
    /// morph the head gain when the offset moved. Call **before** processing
    /// the frame's channels. No-op without active dynamics.
    #[inline]
    pub fn dyn_detect(&mut self, det: f64) {
        if !self.enabled || !self.realizable {
            return;
        }
        let Some(d) = &mut self.dyn_state else {
            return;
        };
        let sc = d.sc_state.process(det, &d.sc_coeffs).abs();
        let coef = if sc > d.env { d.att } else { d.rel };
        d.env += coef * (sc - d.env);
        let over = 20.0 * d.env.max(1e-8).log10() - d.params.threshold_db;
        let target = if over <= 0.0 {
            0.0
        } else {
            over.min(d.params.range_db.abs()) * d.params.range_db.signum()
        };
        if (target - d.offset_db).abs() > 0.01 {
            self.dyn_apply_offset(target);
        }
    }

    /// Detector + filter in one call, for paths where the detector sample is
    /// the processed sample itself (the Mid/Side branch). Identical to
    /// `process_channel` when the band has no active dynamics.
    #[inline]
    pub fn dyn_process(&mut self, sample: f64, channel: usize) -> f64 {
        self.dyn_detect(sample);
        self.process_channel(sample, channel)
    }

    /// Recompute the head peaking coefficients for `gain_db + offset` from the
    /// cached trig — the gain-only morph (mirrors [`BiquadCoeffs::peaking`],
    /// including its `1 + α/A` normalisation).
    fn dyn_apply_offset(&mut self, offset_db: f64) {
        let Some(d) = &mut self.dyn_state else {
            return;
        };
        d.offset_db = offset_db;
        let a = 10f64.powf((self.gain_db + offset_db) / 40.0);
        let a0 = 1.0 + d.alpha / a;
        self.biquad.coeffs = BiquadCoeffs {
            b0: (1.0 + d.alpha * a) / a0,
            b1: (-2.0 * d.cos_w0) / a0,
            b2: (1.0 - d.alpha * a) / a0,
            a1: (-2.0 * d.cos_w0) / a0,
            a2: (1.0 - d.alpha / a) / a0,
        };
    }

    /// Change the slope (12/24/48 dB/oct) and rebuild the section cascade at `sr`.
    /// Leaves the filter untouched on bad coefficients.
    ///
    /// # Errors
    /// Returns [`FilterError`] if the current parameters are not realisable at
    /// `sr` (same conditions as [`Self::update`]).
    pub fn set_slope(&mut self, slope_db_oct: u8, sr: f64) -> Result<(), FilterError> {
        self.slope_db_oct = normalize_slope(slope_db_oct);
        self.update(self.filter_type, self.freq, self.gain_db, self.q, sr)
    }

    pub fn reset(&mut self) {
        self.biquad.reset();
        self.extra.iter_mut().for_each(BiquadFilter::reset);
        // Clear the detector AND restore the static head — a stale morphed
        // coefficient set surviving a reset would misreport the band's gain.
        if let Some(d) = &mut self.dyn_state {
            d.sc_state.reset();
            d.env = 0.0;
            self.dyn_apply_offset(0.0);
        }
    }

    /// Resize per-channel filter state to `channels` (device renegotiation). The
    /// channel *mask* is unchanged — a band targeting only channel 0 still does
    /// after a widen — and existing channels keep their running history.
    pub fn set_channels(&mut self, channels: usize) {
        self.biquad.set_channels(channels);
        self.extra.iter_mut().for_each(|s| s.set_channels(channels));
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
    // float_cmp: asserts the defaulted gain is exactly 0 dB (flat).
    #[allow(clippy::float_cmp)]
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

    // ── Adjustable slopes (12/24/48 dB/oct Butterworth cascade) ─────────────

    fn build_slope(ft: FilterType, freq: f64, gain_db: f64, q: f64, slope: u8) -> ApoFilter {
        ApoFilter::builder()
            .filter_type(ft)
            .freq(freq)
            .gain_db(gain_db)
            .q(q)
            .slope_db_oct(slope)
            .enabled(true)
            .channels(1)
            .sample_rate(SR)
            .build()
            .unwrap()
    }

    #[test]
    // float_cmp: 12 dB/oct must be byte-identical to the pre-slope single biquad.
    #[allow(clippy::float_cmp)]
    fn slope_12db_is_bit_exact_to_single_biquad() {
        // The default/12 dB path must reproduce the original single-biquad output
        // exactly, so existing presets are unchanged.
        let mut sloped = build_slope(FilterType::LowPass, 1000.0, 0.0, 0.707, 12);
        let c = BiquadCoeffs::low_pass(1000.0, 0.707, SR).unwrap();
        let mut state = BiquadState::default();
        for i in 0..1024 {
            let x = (f64::from(i) * 0.05).sin();
            let a = sloped.process_channel(x, 0);
            let b = state.process(x, &c);
            assert_eq!(a.to_bits(), b.to_bits(), "12 dB slope diverged at {i}");
        }
    }

    #[test]
    fn lowpass_slope_rejection_matches_order() {
        // One octave above Fc a Butterworth LP is down ~order·6 dB.
        for (slope, expected) in [(12u8, -12.3), (24, -24.1), (48, -48.2)] {
            let mut f = build_slope(FilterType::LowPass, 1000.0, 0.0, 0.707, slope);
            let g = filter_gain_db(&mut f, 2000.0, SR);
            assert!(
                (g - expected).abs() < 3.0,
                "LP {slope} dB/oct at 2·Fc: got {g:.1} dB, expected ~{expected}"
            );
        }
    }

    #[test]
    fn highpass_slope_rejection_matches_order() {
        // One octave below Fc a Butterworth HP is down ~order·6 dB.
        for (slope, expected) in [(12u8, -12.3), (24, -24.1), (48, -48.2)] {
            let mut f = build_slope(FilterType::HighPass, 1000.0, 0.0, 0.707, slope);
            let g = filter_gain_db(&mut f, 500.0, SR);
            assert!(
                (g - expected).abs() < 3.0,
                "HP {slope} dB/oct at Fc/2: got {g:.1} dB, expected ~{expected}"
            );
        }
    }

    #[test]
    fn lowpass_minus3db_at_fc_all_orders() {
        // Butterworth is maximally flat with the −3 dB point exactly at Fc for
        // every order.
        for slope in [12u8, 24, 48] {
            let mut f = build_slope(FilterType::LowPass, 1000.0, 0.0, 0.707, slope);
            let g = filter_gain_db(&mut f, 1000.0, SR);
            assert!(
                (g - (-3.0)).abs() < 1.0,
                "LP {slope} dB/oct at Fc should be ~−3 dB, got {g:.2}"
            );
        }
    }

    #[test]
    fn high_shelf_reaches_full_gain_all_orders() {
        // A steeper shelf must still reach its full gain in the pass band.
        for slope in [12u8, 24, 48] {
            let mut f = build_slope(FilterType::HighShelf, 1000.0, 6.0, 0.707, slope);
            let g = filter_gain_db(&mut f, 12000.0, SR);
            assert!(
                (g - 6.0).abs() < 1.0,
                "high shelf {slope} dB/oct should reach +6 dB, got {g:.2}"
            );
        }
    }

    #[test]
    fn slope_ignored_for_peaking() {
        // Peaking is single-biquad; a slope value must not change its response.
        let mut a = build_slope(FilterType::Peaking, 1000.0, 6.0, 1.0, 48);
        let g = filter_gain_db(&mut a, 1000.0, SR);
        assert!(
            (g - 6.0).abs() < 0.5,
            "peaking +6 dB unaffected by slope: {g:.2}"
        );
    }

    // ── Dynamic EQ (per-band level-driven gain morph) ───────────────────────

    /// Shared test params: −25 dBFS threshold leaves a wide margin above the
    /// −6 dBFS loud probe and below the out-of-band detector leakage.
    const DP: DynParams = DynParams {
        threshold_db: -25.0,
        range_db: -6.0,
        attack_ms: 5.0,
        release_ms: 150.0,
    };

    fn dyn_build(freq: f64, gain_db: f64, q: f64, p: Option<DynParams>, sr: f64) -> ApoFilter {
        ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(freq)
            .gain_db(gain_db)
            .q(q)
            .dynamics(p)
            .enabled(true)
            .channels(1)
            .sample_rate(sr)
            .build()
            .unwrap()
    }

    /// Drive a mono sine through detector + filter; RMS gain in dB over
    /// `window` (sample indices) relative to the steady sine input RMS.
    fn dyn_windowed_gain_db(
        f: &mut ApoFilter,
        freq: f64,
        amp: f64,
        total: usize,
        window: std::ops::Range<usize>,
        sr: f64,
    ) -> f64 {
        let mut sq = 0.0;
        for i in 0..total {
            let x = amp * (2.0 * PI * freq * i as f64 / sr).sin();
            f.dyn_detect(x);
            let y = f.process_channel(x, 0);
            if window.contains(&i) {
                sq += y * y;
            }
        }
        let orms = (sq / window.len() as f64).sqrt();
        20.0 * (orms / (amp / std::f64::consts::SQRT_2)).log10()
    }

    /// Settled gain: RMS over the last quarter of a 16384-sample drive.
    fn dyn_settled_gain_db(f: &mut ApoFilter, freq: f64, amp: f64, sr: f64) -> f64 {
        let total = 16384;
        dyn_windowed_gain_db(f, freq, amp, total, total * 3 / 4..total, sr)
    }

    #[test]
    fn dyn_below_threshold_matches_static_band() {
        // −40 dBFS probe vs −30 threshold: the morph never engages, so the
        // dynamic band must be bit-identical to its static twin.
        let p = DynParams {
            threshold_db: -30.0,
            ..DP
        };
        let mut dynamic = dyn_build(1000.0, 6.0, 1.0, Some(p), SR);
        let mut fixed = dyn_build(1000.0, 6.0, 1.0, None, SR);
        for i in 0..4096 {
            let x = 0.01 * (2.0 * PI * 1000.0 * f64::from(i) / SR).sin();
            dynamic.dyn_detect(x);
            fixed.dyn_detect(x);
            let a = dynamic.process_channel(x, 0);
            let b = fixed.process_channel(x, 0);
            assert_eq!(a.to_bits(), b.to_bits(), "diverged at sample {i}");
        }
    }

    #[test]
    fn dyn_full_morph_reaches_range() {
        // −6 dBFS probe overshoots the −25 threshold by 19 dB ≫ |range| = 6,
        // so the settled gain hits the full range cut.
        let mut f = dyn_build(1000.0, 0.0, 1.0, Some(DP), SR);
        let g = dyn_settled_gain_db(&mut f, 1000.0, 0.5, SR);
        assert!((g - (-6.0)).abs() < 0.5, "full morph: got {g:.2} dB");
    }

    #[test]
    fn dyn_partial_morph_tracks_overshoot() {
        // Probe at −27 dBFS over a −30 threshold = 3 dB overshoot → −3 dB
        // offset (1:1 growth below the range cap).
        let p = DynParams {
            threshold_db: -30.0,
            ..DP
        };
        let mut f = dyn_build(1000.0, 0.0, 1.0, Some(p), SR);
        let amp = 10f64.powf(-27.0 / 20.0);
        let g = dyn_settled_gain_db(&mut f, 1000.0, amp, SR);
        assert!((g - (-3.0)).abs() < 0.7, "partial morph: got {g:.2} dB");
    }

    #[test]
    fn dyn_positive_range_boosts_when_loud() {
        let p = DynParams {
            range_db: 6.0,
            ..DP
        };
        let mut f = dyn_build(1000.0, 0.0, 1.0, Some(p), SR);
        let g = dyn_settled_gain_db(&mut f, 1000.0, 0.5, SR);
        assert!((g - 6.0).abs() < 0.5, "positive range: got {g:.2} dB");
    }

    #[test]
    fn dyn_attack_setting_controls_engage_speed() {
        // 10–20 ms after a loud onset, a 2 ms attack has mostly morphed while
        // a 200 ms attack has barely moved.
        let fast = DynParams {
            attack_ms: 2.0,
            ..DP
        };
        let slow = DynParams {
            attack_ms: 200.0,
            ..DP
        };
        let win = 480..960; // 10–20 ms at 48 kHz
        let mut ff = dyn_build(1000.0, 0.0, 1.0, Some(fast), SR);
        let gf = dyn_windowed_gain_db(&mut ff, 1000.0, 0.5, 960, win.clone(), SR);
        let mut fs = dyn_build(1000.0, 0.0, 1.0, Some(slow), SR);
        let gs = dyn_windowed_gain_db(&mut fs, 1000.0, 0.5, 960, win, SR);
        assert!(
            gf < -4.0,
            "fast attack should be mostly engaged: {gf:.2} dB"
        );
        assert!(gs > -2.0, "slow attack should barely move: {gs:.2} dB");
    }

    #[test]
    fn dyn_release_setting_controls_recovery_speed() {
        // Loud phase, then a −60 dBFS probe phase: 30–40 ms in, a 10 ms
        // release has recovered while a 2000 ms release still holds the cut.
        let drive = |release_ms: f64| {
            let p = DynParams {
                attack_ms: 2.0,
                release_ms,
                ..DP
            };
            let mut f = dyn_build(1000.0, 0.0, 1.0, Some(p), SR);
            // loud phase — fully engage
            dyn_settled_gain_db(&mut f, 1000.0, 0.5, SR);
            // quiet phase — measure 30–40 ms in
            dyn_windowed_gain_db(&mut f, 1000.0, 0.001, 1920, 1440..1920, SR)
        };
        let recovered = drive(10.0);
        let held = drive(2000.0);
        assert!(recovered > -1.0, "fast release: got {recovered:.2} dB");
        assert!(held < -4.0, "slow release: got {held:.2} dB");
    }

    #[test]
    fn dyn_out_of_band_signal_does_not_trigger() {
        // A loud 8 kHz tone is 4 octaves above a 500 Hz / Q 2 band — the BP
        // sidechain rejects it, so the coefficients are never touched and the
        // output matches the static twin bit-for-bit.
        let mut dynamic = dyn_build(500.0, 0.0, 2.0, Some(DP), SR);
        let mut fixed = dyn_build(500.0, 0.0, 2.0, None, SR);
        for i in 0..8192 {
            let x = 0.5 * (2.0 * PI * 8000.0 * f64::from(i) / SR).sin();
            dynamic.dyn_detect(x);
            fixed.dyn_detect(x);
            let a = dynamic.process_channel(x, 0);
            let b = fixed.process_channel(x, 0);
            assert_eq!(a.to_bits(), b.to_bits(), "out-of-band morph at {i}");
        }
    }

    #[test]
    fn dyn_params_clamped_and_nonfinite_rejected() {
        let hostile = DynParams {
            threshold_db: -200.0,
            range_db: 100.0,
            attack_ms: -5.0,
            release_ms: f64::NAN,
        };
        let f = dyn_build(1000.0, 0.0, 1.0, Some(hostile), SR);
        let p = f.dynamics().expect("dynamics should be attached");
        assert!((p.threshold_db - (-80.0)).abs() < 1e-12);
        assert!((p.range_db - 24.0).abs() < 1e-12);
        assert!((p.attack_ms - 0.1).abs() < 1e-12);
        assert!(
            (p.release_ms - DynParams::DEFAULT.release_ms).abs() < 1e-12,
            "non-finite release falls back to the default"
        );
    }

    #[test]
    fn dyn_only_on_peaking() {
        // Non-peaking types silently ignore dynamics (front-ends gate the UI).
        let shelf = ApoFilter::builder()
            .filter_type(FilterType::LowShelf)
            .freq(1000.0)
            .gain_db(6.0)
            .q(0.707)
            .dynamics(Some(DynParams::DEFAULT))
            .enabled(true)
            .channels(1)
            .sample_rate(SR)
            .build()
            .unwrap();
        assert!(shelf.dynamics().is_none());

        let mut peak = dyn_build(1000.0, 0.0, 1.0, None, SR);
        assert!(peak.dynamics().is_none());
        peak.set_dynamics(Some(DynParams::DEFAULT), SR).unwrap();
        assert!(peak.dynamics().is_some());
        peak.set_dynamics(None, SR).unwrap();
        assert!(peak.dynamics().is_none());
    }

    #[test]
    fn dyn_survives_rebind() {
        // Rebinding to a new rate re-derives the detector; the morph still
        // lands and the output stays finite.
        let mut f = dyn_build(1000.0, 0.0, 1.0, Some(DP), SR);
        f.rebind(96_000.0);
        let g = dyn_settled_gain_db(&mut f, 1000.0, 0.5, 96_000.0);
        assert!(
            (g - (-6.0)).abs() < 0.7,
            "morph after rebind: got {g:.2} dB"
        );
        assert!(f.dynamics().is_some(), "dynamics survive the rebind");
    }

    #[test]
    fn dyn_reset_restores_static_response() {
        // After a full morph, reset() must clear the envelope AND restore the
        // static coefficients — a stale morphed head after reset is a bug.
        let mut f = dyn_build(1000.0, 0.0, 1.0, Some(DP), SR);
        dyn_settled_gain_db(&mut f, 1000.0, 0.5, SR); // engage
        f.reset();
        let mut fixed = dyn_build(1000.0, 0.0, 1.0, None, SR);
        // quiet probe below threshold: both must be bit-identical from rest
        for i in 0..2048 {
            let x = 0.01 * (2.0 * PI * 1000.0 * f64::from(i) / SR).sin();
            f.dyn_detect(x);
            fixed.dyn_detect(x);
            let a = f.process_channel(x, 0);
            let b = fixed.process_channel(x, 0);
            assert_eq!(a.to_bits(), b.to_bits(), "reset left morphed state at {i}");
        }
    }
}
