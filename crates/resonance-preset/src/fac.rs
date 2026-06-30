use crate::model::{ApoFilterType, EffectState, EqBand, FxEffects, Preset};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FacError {
    #[error("missing magic header (expected 'CLASS1 : Effect Type')")]
    MissingMagic,
    #[error("unexpected end of file at line {0}")]
    UnexpectedEof(usize),
    #[error("parse error at line {line}: {msg}")]
    ParseError { line: usize, msg: String },
}

/// Upper bound on any element/value count read from a `.fac` file before it
/// drives a skip loop or an allocation. A hostile or garbled count must not be
/// able to spin the parser, overflow an `i32` multiply, or abort the process on
/// an oversized allocation.
const MAX_COUNT: i32 = 100_000;

/// Upper bound on the EQ band count specifically. Real presets have ~10 bands;
/// this caps `Vec::with_capacity` so a corrupt count can't request a huge
/// allocation.
const MAX_BANDS: i32 = 1024;

/// Number of effect knobs (`Main 0`–`Main 5`): Fidelity, Surround, unused,
/// Ambience, `DynamicBoost`, `BassBoost`.
const MAIN_KNOBS: usize = 6;

/// Number of leading application-dependent integers we model (the per-effect
/// on/off flags). Extra integers beyond these are consumed but ignored.
const APP_INT_FLAGS: usize = 7;

/// Line-oriented cursor over a `.fac` file's text.
///
/// Wraps the enumerated `lines()` iterator and tracks the most recently read
/// line number so an unexpected EOF reports the line it was expected on (the
/// behaviour the parser relies on for its error messages).
struct FacReader<'a> {
    lines: std::iter::Enumerate<std::str::Lines<'a>>,
    /// 1-based number of the last line returned, used as the EOF fallback for
    /// the *next* read.
    last_line: usize,
}

impl<'a> FacReader<'a> {
    fn new(content: &'a str) -> Self {
        Self {
            lines: content.lines().enumerate(),
            // An immediate EOF (empty file) reports line 1 — the first line the
            // parser expects (the magic header).
            last_line: 1,
        }
    }

    /// Read the next line, trimmed, returning its 1-based number.
    ///
    /// # Errors
    ///
    /// [`FacError::UnexpectedEof`] (carrying the last line seen) when the file
    /// ends before the required line.
    fn next_line(&mut self) -> Result<(usize, &'a str), FacError> {
        match self.lines.next() {
            Some((n, s)) => {
                let ln = n + 1;
                self.last_line = ln;
                Ok((ln, s.trim()))
            }
            None => Err(FacError::UnexpectedEof(self.last_line)),
        }
    }

    /// Read and discard the next line (skip a header/padding line).
    ///
    /// # Errors
    ///
    /// [`FacError::UnexpectedEof`] when the file ends first.
    fn skip_line(&mut self) -> Result<(), FacError> {
        self.next_line().map(|_| ())
    }

    /// Read the next line and parse its leading `"value: label"` integer.
    fn next_int(&mut self) -> Result<i32, FacError> {
        let (ln, line) = self.next_line()?;
        parse_prefixed_int(line, ln)
    }

    /// Read a count line and reject implausible values before it can drive a
    /// skip loop, an `i32` multiply, or an allocation. `max` bounds the count
    /// and `what` names it for the error message.
    fn next_count(&mut self, max: i32, what: &str) -> Result<i32, FacError> {
        let (ln, line) = self.next_line()?;
        let n = parse_prefixed_int(line, ln)?;
        if !(0..=max).contains(&n) {
            return Err(FacError::ParseError {
                line: ln,
                msg: format!("implausible {what} count {n}"),
            });
        }
        Ok(n)
    }

    /// Read the next *numeric-prefixed* value, skipping non-numeric header lines
    /// (e.g. "Band 1") that real `FxSound` files interleave between values.
    ///
    /// # Errors
    ///
    /// [`FacError::UnexpectedEof`] when the file ends before a numeric line.
    fn next_value(&mut self) -> Result<f64, FacError> {
        loop {
            let (_, line) = self.next_line()?;
            if let Some(v) = numeric_prefix(line) {
                return Ok(v);
            }
        }
    }
}

/// The effect-knob block: `Main 0`–`Main 5` MIDI values plus the per-effect
/// on/off flags read later from the application-dependent integers.
struct KnobBlock {
    /// `Main 0`–`Main 5` MIDI knob values (0–127).
    main: [i32; MAIN_KNOBS],
    /// Leading application-dependent integers — the per-effect enable flags.
    app_ints: [i32; APP_INT_FLAGS],
}

/// Read and validate the magic header plus the fixed preamble lines, returning
/// the preset name (line 3). Consumes lines 1–5: magic, version, name, double-
/// params flag, then the total-element count, whose element block is skipped.
fn parse_header(r: &mut FacReader) -> Result<String, FacError> {
    // Line 1: magic.
    let (_, magic) = r.next_line()?;
    if magic != "CLASS1 : Effect Type" {
        return Err(FacError::MissingMagic);
    }

    r.skip_line()?; // Line 2: version (e.g. "9: Version").
    let (_, name_line) = r.next_line()?; // Line 3: preset name.
    let name = name_line.to_string();
    r.skip_line()?; // Line 4: double-params flag.

    Ok(name)
}

/// Skip the per-element block: a "0: Element Number" line followed by 7 param
/// lines, repeated `total_elements` times.
///
/// In the file layout this block sits *after* the `Main` knobs (which follow the
/// element-count line), so the caller reads the count, then the knobs, then
/// calls this with the count.
fn skip_element_block(r: &mut FacReader, total_elements: i32) -> Result<(), FacError> {
    // "0: Element Number" + 7 param lines per element.
    r.skip_line()?;
    for _ in 0..(total_elements * 7) {
        r.skip_line()?;
    }
    Ok(())
}

/// Read the six `Main N` MIDI knob values (Fidelity, Surround, unused, Ambience,
/// `DynamicBoost`, `BassBoost`), which immediately follow the element-count line.
fn read_main_knobs(r: &mut FacReader) -> Result<[i32; MAIN_KNOBS], FacError> {
    let mut main = [0i32; MAIN_KNOBS];
    for slot in &mut main {
        *slot = r.next_int()?;
    }
    Ok(main)
}

/// Read the application-dependent section that follows the element block: the
/// integer/real/string counts, then the integer flags (the first 7 are the
/// per-effect on/off flags; any extras are consumed but ignored), then skip the
/// declared reals and strings so the EQ section that follows stays line-aligned.
fn read_app_section(r: &mut FacReader) -> Result<[i32; APP_INT_FLAGS], FacError> {
    // Counts of application-dependent values (integers / reals / strings).
    let num_ints = r.next_count(MAX_COUNT, "app-dependent integer")? as usize;
    let num_reals = r.next_count(MAX_COUNT, "app-dependent real")? as usize;
    let num_strings = r.next_count(MAX_COUNT, "app-dependent string")? as usize;

    let mut app_ints = [0i32; APP_INT_FLAGS];
    for i in 0..num_ints {
        let v = r.next_int()?;
        if let Some(slot) = app_ints.get_mut(i) {
            *slot = v;
        }
    }

    // Reals and strings are unused, but they occupy lines the EQ section would
    // otherwise be misread from.
    for _ in 0..(num_reals + num_strings) {
        r.skip_line()?;
    }

    Ok(app_ints)
}

/// Parse the EQ section: band count, on/off flag, then a frequency + gain pair
/// per band. Returns the bands and whether the EQ is enabled. Each value skips
/// any non-numeric header lines ("Band 1", "Band 2", …) that real `FxSound`
/// files prefix bands with.
fn parse_eq_block(r: &mut FacReader) -> Result<(Vec<EqBand>, bool), FacError> {
    let num_bands = r.next_count(MAX_BANDS, "band")? as usize;
    let eq_enabled = r.next_int()? != 0;

    let mut bands = Vec::with_capacity(num_bands);
    for _ in 0..num_bands {
        let freq = r.next_value()?;
        let gain_db = r.next_value()?;
        bands.push(EqBand {
            filter_type: ApoFilterType::Peaking,
            freq,
            gain_db,
            // Q stays 1.41 for the graphic EQ regardless of stored CFs (see CLAUDE.md).
            q: 1.41,
            enabled: true,
            channels: u64::MAX,
        });
    }

    Ok((bands, eq_enabled))
}

/// Assemble the `FxSound` effect chain from the knob block. MIDI knob values
/// (0–127) normalise to 0.0–1.0 intensity; each effect's enable comes from its
/// application-dependent integer flag. Note the `Main`→effect mapping skips
/// `Main 2` (unused) and pairs Ambience with `Main 3`.
fn assemble_effects(knobs: &KnobBlock) -> FxEffects {
    let midi_norm = |v: i32| f64::from(v) / 127.0;
    let main = &knobs.main;
    let flags = &knobs.app_ints;

    FxEffects {
        fidelity: EffectState {
            enabled: flags[0] != 0,
            intensity: midi_norm(main[0]),
        },
        surround: EffectState {
            enabled: flags[1] != 0,
            intensity: midi_norm(main[1]),
        },
        ambience: EffectState {
            enabled: flags[2] != 0,
            intensity: midi_norm(main[3]),
        },
        dynamic_boost: EffectState {
            enabled: flags[3] != 0,
            intensity: midi_norm(main[4]),
        },
        bass: EffectState {
            enabled: flags[4] != 0,
            intensity: midi_norm(main[5]),
        },
    }
}

/// Parse a `FxSound` .fac preset file from its text content.
///
/// # Errors
///
/// Returns [`FacError::MissingMagic`] when the file lacks the expected header,
/// [`FacError::UnexpectedEof`] when it ends before a required field, or
/// [`FacError::ParseError`] when a field cannot be parsed.
pub fn parse_fac(content: &str) -> Result<Preset, FacError> {
    let mut r = FacReader::new(content);

    let name = parse_header(&mut r)?;
    // File order: element-count line, then the Main knobs, then the element
    // block, then the application-dependent section. Reading the knobs *before*
    // skipping the element block is essential — they sit between the two.
    let total_elements = r.next_count(MAX_COUNT, "element")?;
    let main = read_main_knobs(&mut r)?;
    skip_element_block(&mut r, total_elements)?;
    let app_ints = read_app_section(&mut r)?;
    let knobs = KnobBlock { main, app_ints };
    let (bands, eq_enabled) = parse_eq_block(&mut r)?;
    let effects = assemble_effects(&knobs);

    Ok(Preset {
        name,
        preamp_db: 0.0,
        eq_enabled,
        bands,
        effects,
    })
}

/// Parse the leading `"value: label"` integer prefix of a line.
fn parse_prefixed_int(line: &str, ln: usize) -> Result<i32, FacError> {
    let val = line.split(':').next().unwrap_or("").trim();
    val.parse::<i32>().map_err(|_| FacError::ParseError {
        line: ln,
        msg: format!("expected integer prefix, got '{line}'"),
    })
}

/// Parse the leading "value: label" number, returning `None` for header lines
/// (e.g. "Band 1") whose prefix isn't numeric. Non-finite prefixes ("nan"/"inf",
/// which `f64::parse` accepts) are rejected so they can't poison a band's
/// frequency or gain — the line is treated as a non-value and skipped.
fn numeric_prefix(line: &str) -> Option<f64> {
    line.split(':')
        .next()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    const POP_FAC: &str = "CLASS1 : Effect Type\n\
        9: Version\n\
        Pop\n\
        0: Double Params Flag\n\
        1: Total number of elements\n\
        38: Main 0\n\
        0: Main 1\n\
        0: Main 2\n\
        13: Main 3\n\
        89: Main 4\n\
        25: Main 5\n\
        0: Element Number\n\
           0: Param 0\n\
           0: Param 1\n\
           0: Param 2\n\
           0: Param 3\n\
           0: Param 4\n\
           0: Param 5\n\
           0: Param 6\n\
        7: Number of Application Dependent Integers\n\
        0: Number of Application Dependent Reals\n\
        0: Number of Application Dependent Strings\n\
        1: Integer[0]\n\
        1: Integer[1]\n\
        1: Integer[2]\n\
        1: Integer[3]\n\
        1: Integer[4]\n\
        0: Integer[5]\n\
        2: Integer[6]\n\
        10: Number of EQ Bands\n\
        1: On/Off\n\
        62.5: Band 0 CF\n\
        1.97: Band 0 Boost\n\
        110: Band 1 CF\n\
        0: Band 1 Boost\n\
        230: Band 2 CF\n\
        0: Band 2 Boost\n\
        370: Band 3 CF\n\
        0: Band 3 Boost\n\
        650: Band 4 CF\n\
        0: Band 4 Boost\n\
        1200: Band 5 CF\n\
        0: Band 5 Boost\n\
        2150: Band 6 CF\n\
        0: Band 6 Boost\n\
        5300: Band 7 CF\n\
        0: Band 7 Boost\n\
        10000: Band 8 CF\n\
        0: Band 8 Boost\n\
        12000: Band 9 CF\n\
        0: Band 9 Boost\n";

    #[test]
    fn parse_pop_preset() {
        let preset = parse_fac(POP_FAC).unwrap();
        assert_eq!(preset.name, "Pop");
        assert_eq!(preset.bands.len(), 10);
        assert!((preset.bands[0].freq - 62.5).abs() < 0.01);
        assert!((preset.effects.fidelity.intensity - 38.0 / 127.0).abs() < 0.001);
        assert!(preset.effects.fidelity.enabled);
        assert!(preset.eq_enabled);
    }

    // Real FxSound files prefix each band with a "Band N" header line.
    const REAL_FAC: &str = "CLASS1 : Effect Type\n\
        9: Version\n\
        Pop\n\
        0: Double Params Flag\n\
        1: Total number of elements\n\
        38: Main 0\n\
        0: Main 1\n\
        0: Main 2\n\
        13: Main 3\n\
        89: Main 4\n\
        25: Main 5\n\
        0: Element Number\n\
           0: Param 0\n\
           0: Param 1\n\
           0: Param 2\n\
           0: Param 3\n\
           0: Param 4\n\
           0: Param 5\n\
           0: Param 6\n\
        7: Number of Application Dependent Integers\n\
        0: Number of Application Dependent Reals\n\
        0: Number of Application Dependent Strings\n\
        1: Integer[0]\n\
        1: Integer[1]\n\
        1: Integer[2]\n\
        1: Integer[3]\n\
        1: Integer[4]\n\
        0: Integer[5]\n\
        2: Integer[6]\n\
        3: Number of EQ Bands\n\
        1: On/Off Flag\n\
        Band 1\n\
           62.5: CF\n\
           1.9685: Boost/Cut\n\
        Band 2\n\
           110: CF\n\
           0: Boost/Cut\n\
        Band 3\n\
           230: CF\n\
           -2.5: Boost/Cut\n";

    #[test]
    fn parse_real_format_with_band_headers() {
        let preset = parse_fac(REAL_FAC).unwrap();
        assert_eq!(preset.bands.len(), 3);
        assert!((preset.bands[0].freq - 62.5).abs() < 0.01);
        assert!((preset.bands[0].gain_db - 1.9685).abs() < 0.01);
        assert!((preset.bands[2].freq - 230.0).abs() < 0.01);
        assert!((preset.bands[2].gain_db + 2.5).abs() < 0.01);
    }

    #[test]
    fn missing_magic_is_rejected() {
        let err = parse_fac("not the magic\n").unwrap_err();
        assert!(matches!(err, FacError::MissingMagic));
    }

    #[test]
    fn truncated_file_reports_unexpected_eof() {
        // Magic only: the next required line (version) is missing.
        let err = parse_fac("CLASS1 : Effect Type\n").unwrap_err();
        assert!(matches!(err, FacError::UnexpectedEof(1)), "got {err:?}");
    }

    #[test]
    fn implausible_band_count_is_rejected() {
        // A band count beyond MAX_BANDS is rejected before any band is read, so
        // the file can stop right after the count line.
        let s = REAL_FAC.replace("3: Number of EQ Bands", "99999: Number of EQ Bands");
        let err = parse_fac(&s).unwrap_err();
        assert!(matches!(err, FacError::ParseError { .. }), "got {err:?}");
    }

    // float_cmp: MIDI→intensity normalisation is exact division by a constant.
    #[allow(clippy::float_cmp)]
    #[test]
    fn assemble_effects_maps_main_and_flags() {
        let knobs = KnobBlock {
            // Surround=Main 1, Ambience=Main 3 (Main 2 unused), DynBoost=Main 4, Bass=Main 5.
            main: [127, 64, 0, 32, 16, 8],
            app_ints: [1, 0, 1, 0, 1, 0, 0],
        };
        let fx = assemble_effects(&knobs);
        assert_eq!(fx.fidelity.intensity, 127.0 / 127.0);
        assert!(fx.fidelity.enabled);
        assert_eq!(fx.surround.intensity, 64.0 / 127.0);
        assert!(!fx.surround.enabled);
        // Ambience reads Main 3, not Main 2.
        assert_eq!(fx.ambience.intensity, 32.0 / 127.0);
        assert!(fx.ambience.enabled);
        assert_eq!(fx.dynamic_boost.intensity, 16.0 / 127.0);
        assert!(!fx.dynamic_boost.enabled);
        assert_eq!(fx.bass.intensity, 8.0 / 127.0);
        assert!(fx.bass.enabled);
    }

    // float_cmp: numeric_prefix parses an exact decimal literal; non-finite rejected.
    #[allow(clippy::float_cmp)]
    #[test]
    fn numeric_prefix_parses_value_and_rejects_headers_and_nonfinite() {
        assert_eq!(numeric_prefix("62.5: CF"), Some(62.5));
        assert_eq!(numeric_prefix("-2.5: Boost/Cut"), Some(-2.5));
        // Header lines have no numeric prefix.
        assert_eq!(numeric_prefix("Band 1"), None);
        // nan/inf parse as f64 but are rejected as non-values.
        assert_eq!(numeric_prefix("nan: x"), None);
        assert_eq!(numeric_prefix("inf: x"), None);
    }
}
