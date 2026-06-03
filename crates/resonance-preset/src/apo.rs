use crate::model::{ApoFilterType, EqBand, Preset};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApoError {
    #[error("parse error at line {line}: {msg}")]
    ParseError { line: usize, msg: String },
}

/// Parse an EqualizerAPO .txt config file.
/// Supported directives: Preamp, Filter (ON/OFF PK/LS/HS/LP/HP/BP/NO/AP), GraphicEQ, Channel, Include (ignored).
pub fn parse_apo(content: &str) -> Result<Preset, ApoError> {
    let mut bands = Vec::new();
    let mut preamp_db = 0.0f64;

    for (ln0, line) in content.lines().enumerate() {
        let ln = ln0 + 1;
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("Preamp:") {
            preamp_db = parse_db(rest.trim(), ln)?;
            continue;
        }

        if line.starts_with("Filter") {
            if let Some(band) = parse_filter_line(line, ln)? {
                bands.push(band);
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("GraphicEQ:") {
            let graphic_bands = parse_graphic_eq(rest.trim(), ln)?;
            bands.extend(graphic_bands);
            continue;
        }
        // Channel, Include, and unknown directives are silently skipped.
    }

    Ok(Preset {
        name: String::from("APO Import"),
        preamp_db,
        eq_enabled: !bands.is_empty(),
        bands,
        effects: Default::default(),
    })
}

fn parse_filter_line(line: &str, ln: usize) -> Result<Option<EqBand>, ApoError> {
    let mut tokens = line.split_whitespace().peekable();
    tokens.next(); // "Filter"

    // optional index: "Filter 1:" or "Filter:"
    if let Some(t) = tokens.peek() {
        if t.ends_with(':') || t.parse::<u32>().is_ok() {
            tokens.next();
            // also consume trailing colon token if present
            if let Some(t) = tokens.peek() {
                if *t == ":" {
                    tokens.next();
                }
            }
        }
    }

    let enabled = match tokens.next() {
        Some("ON") => true,
        Some("OFF") => false,
        _ => return Ok(None),
    };

    let filter_type_str = tokens.next().unwrap_or("");
    let filter_type = parse_filter_type(filter_type_str, ln)?;

    let mut freq = 1000.0f64;
    let mut gain_db = 0.0f64;
    let mut q = 0.707f64;

    while let Some(key) = tokens.next() {
        match key {
            "Fc" => {
                freq = tokens
                    .next()
                    .ok_or_else(|| err(ln, "expected value after Fc"))?
                    .parse::<f64>()
                    .map_err(|_| err(ln, "invalid Fc value"))?;
                tokens.next(); // consume "Hz"
            }
            "Gain" => {
                gain_db = tokens
                    .next()
                    .ok_or_else(|| err(ln, "expected value after Gain"))?
                    .parse::<f64>()
                    .map_err(|_| err(ln, "invalid Gain value"))?;
                tokens.next(); // consume "dB"
            }
            "Q" => {
                q = tokens
                    .next()
                    .ok_or_else(|| err(ln, "expected value after Q"))?
                    .parse::<f64>()
                    .map_err(|_| err(ln, "invalid Q value"))?;
            }
            _ => {}
        }
    }

    Ok(Some(EqBand {
        filter_type,
        freq,
        gain_db,
        q,
        enabled,
    }))
}

fn parse_filter_type(s: &str, ln: usize) -> Result<ApoFilterType, ApoError> {
    match s {
        "PK" => Ok(ApoFilterType::Peaking),
        "LS" => Ok(ApoFilterType::LowShelf),
        "LS 12dB" => Ok(ApoFilterType::LowShelf12Db),
        "LSC" => Ok(ApoFilterType::LowShelfQ),
        "HS" => Ok(ApoFilterType::HighShelf),
        "HS 12dB" => Ok(ApoFilterType::HighShelf12Db),
        "HSC" => Ok(ApoFilterType::HighShelfQ),
        "LP" => Ok(ApoFilterType::LowPass),
        "LPQ" => Ok(ApoFilterType::LowPassQ),
        "HP" => Ok(ApoFilterType::HighPass),
        "HPQ" => Ok(ApoFilterType::HighPassQ),
        "BP" => Ok(ApoFilterType::BandPass),
        "NO" => Ok(ApoFilterType::Notch),
        "AP" => Ok(ApoFilterType::AllPass),
        _ => Err(err(ln, &format!("unknown filter type '{s}'"))),
    }
}

fn parse_graphic_eq(s: &str, ln: usize) -> Result<Vec<EqBand>, ApoError> {
    // Format: "20 0; 25 -1.2; 31 0.5; ..."
    s.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|pair| {
            let mut parts = pair.trim().split_whitespace();
            let freq = parts
                .next()
                .ok_or_else(|| err(ln, "missing freq in GraphicEQ"))?
                .parse::<f64>()
                .map_err(|_| err(ln, "invalid freq in GraphicEQ"))?;
            let gain_db = parts
                .next()
                .ok_or_else(|| err(ln, "missing gain in GraphicEQ"))?
                .parse::<f64>()
                .map_err(|_| err(ln, "invalid gain in GraphicEQ"))?;
            Ok(EqBand {
                filter_type: ApoFilterType::Peaking,
                freq,
                gain_db,
                q: 1.41,
                enabled: true,
            })
        })
        .collect()
}

fn parse_db(s: &str, ln: usize) -> Result<f64, ApoError> {
    let val = s.trim_end_matches("dB").trim();
    val.parse::<f64>().map_err(|_| err(ln, "invalid dB value"))
}

fn err(line: usize, msg: &str) -> ApoError {
    ApoError::ParseError {
        line,
        msg: msg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_apo() {
        let content = "Preamp: -6 dB\nFilter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.41\nFilter 2: ON HS Fc 10000 Hz Gain 2.0 dB\n";
        let preset = parse_apo(content).unwrap();
        assert!((preset.preamp_db - (-6.0)).abs() < 0.001);
        assert_eq!(preset.bands.len(), 2);
        assert_eq!(preset.bands[0].filter_type, ApoFilterType::Peaking);
        assert!((preset.bands[0].freq - 1000.0).abs() < 0.01);
        assert!((preset.bands[0].gain_db - (-3.0)).abs() < 0.001);
    }

    #[test]
    fn parse_graphic_eq() {
        let content = "GraphicEQ: 20 0; 100 -2.5; 1000 1.0\n";
        let preset = parse_apo(content).unwrap();
        assert_eq!(preset.bands.len(), 3);
        assert!((preset.bands[1].freq - 100.0).abs() < 0.01);
        assert!((preset.bands[1].gain_db - (-2.5)).abs() < 0.001);
    }
}
