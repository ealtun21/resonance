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

/// Parse a FxSound .fac preset file from its text content.
pub fn parse_fac(content: &str) -> Result<Preset, FacError> {
    let mut lines = content.lines().enumerate().peekable();

    let mut next = |expected_line: usize| -> Result<(usize, &str), FacError> {
        match lines.next() {
            Some((n, s)) => Ok((n + 1, s.trim())),
            None => Err(FacError::UnexpectedEof(expected_line)),
        }
    };

    // Line 1: magic
    let (ln, magic) = next(1)?;
    if magic != "CLASS1 : Effect Type" {
        return Err(FacError::MissingMagic);
    }

    // Line 2: version (e.g. "9: Version")
    let (ln, _) = next(ln)?;

    // Line 3: preset name
    let (ln, name_line) = next(ln)?;
    let name = name_line.to_string();

    // Line 4: double params flag ("0: Double Params Flag")
    let (ln, _) = next(ln)?;

    // Line 5: total number of elements ("1: Total number of elements")
    let (ln, total_elements_line) = next(ln)?;
    let total_elements = parse_prefixed_int(total_elements_line, ln)?;

    // Lines 6–11: Main 0–5 (Fidelity, Surround, unused, Ambience, DynamicBoost, BassBoost)
    let mut main = [0i32; 6];
    let mut ln = ln;
    for slot in &mut main {
        let (next_ln, line) = next(ln)?;
        ln = next_ln;
        *slot = parse_prefixed_int(line, ln)?;
    }

    // Skip element block: "0: Element Number" + 7 params lines
    let (next_ln, _) = next(ln)?;
    ln = next_ln;
    for _ in 0..(total_elements * 7) {
        let (next_ln, _) = next(ln)?;
        ln = next_ln;
    }

    // "7: Number of Application Dependent Integers"
    let (next_ln, num_ints_line) = next(ln)?;
    ln = next_ln;
    let num_ints = parse_prefixed_int(num_ints_line, ln)?;

    // "0: Number of Application Dependent Reals"
    let (next_ln, num_reals_line) = next(ln)?;
    ln = next_ln;
    let _num_reals = parse_prefixed_int(num_reals_line, ln)?;

    // "0: Number of Application Dependent Strings"
    let (next_ln, num_strings_line) = next(ln)?;
    ln = next_ln;
    let _num_strings = parse_prefixed_int(num_strings_line, ln)?;

    // Read app-depend integers (expect 7)
    let mut app_ints = [0i32; 7];
    for slot in app_ints.iter_mut().take((num_ints as usize).min(7)) {
        let (next_ln, line) = next(ln)?;
        ln = next_ln;
        *slot = parse_prefixed_int(line, ln)?;
    }

    // EQ section
    let (next_ln, num_bands_line) = next(ln)?;
    ln = next_ln;
    let num_bands = parse_prefixed_int(num_bands_line, ln)? as usize;

    let (next_ln, eq_on_line) = next(ln)?;
    ln = next_ln;
    let eq_enabled = parse_prefixed_int(eq_on_line, ln)? != 0;

    // Each band is a "CF" line then a "Boost/Cut" line. Real FxSound files also
    // prefix each band with a non-numeric header line ("Band 1", "Band 2", …),
    // so skip any line whose prefix isn't a number before reading each value.
    let mut read_value = |ln: &mut usize| -> Result<f64, FacError> {
        loop {
            let (n, line) = next(*ln)?;
            *ln = n;
            if let Some(v) = numeric_prefix(line) {
                return Ok(v);
            }
        }
    };

    let mut bands = Vec::with_capacity(num_bands);
    for _ in 0..num_bands {
        let freq = read_value(&mut ln)?;
        let gain_db = read_value(&mut ln)?;
        bands.push(EqBand {
            filter_type: ApoFilterType::Peaking,
            freq,
            gain_db,
            q: 1.41,
            enabled: true,
        });
    }

    // MIDI 0–127 → 0.0–1.0
    let midi_norm = |v: i32| v as f64 / 127.0;

    let effects = FxEffects {
        fidelity: EffectState {
            enabled: app_ints[0] != 0,
            intensity: midi_norm(main[0]),
        },
        surround: EffectState {
            enabled: app_ints[1] != 0,
            intensity: midi_norm(main[1]),
        },
        ambience: EffectState {
            enabled: app_ints[2] != 0,
            intensity: midi_norm(main[3]),
        },
        dynamic_boost: EffectState {
            enabled: app_ints[3] != 0,
            intensity: midi_norm(main[4]),
        },
        bass: EffectState {
            enabled: app_ints[4] != 0,
            intensity: midi_norm(main[5]),
        },
    };

    Ok(Preset {
        name,
        preamp_db: 0.0,
        eq_enabled,
        bands,
        effects,
    })
}

fn parse_prefixed_int(line: &str, ln: usize) -> Result<i32, FacError> {
    let val = line.split(':').next().unwrap_or("").trim();
    val.parse::<i32>().map_err(|_| FacError::ParseError {
        line: ln,
        msg: format!("expected integer prefix, got '{line}'"),
    })
}

/// Parse the leading "value: label" number, returning `None` for header lines
/// (e.g. "Band 1") whose prefix isn't numeric.
fn numeric_prefix(line: &str) -> Option<f64> {
    line.split(':').next()?.trim().parse::<f64>().ok()
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
}
