use crate::model::{ApoFilterType, EqBand, Preset};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApoError {
    #[error("parse error at line {line}: {msg}")]
    ParseError { line: usize, msg: String },
}

/// Parse an EqualizerAPO .txt config file.
/// Supported directives: Preamp, Filter (ON/OFF PK/LS/HS/LP/HP/BP/NO/AP),
/// GraphicEQ, Channel (per-channel targeting); Include is ignored.
pub fn parse_apo(content: &str) -> Result<Preset, ApoError> {
    let mut bands = Vec::new();
    let mut preamp_db = 0.0f64;
    // A `Channel:` directive scopes every following Filter/GraphicEQ to a subset
    // of channels until the next `Channel:` line. Default: all channels.
    let mut current_channels = u64::MAX;

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

        if let Some(rest) = line.strip_prefix("Channel:") {
            current_channels = parse_channel_line(rest);
            continue;
        }

        if line.starts_with("Filter") {
            if let Some(band) = parse_filter_line(line, ln, current_channels)? {
                bands.push(band);
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("GraphicEQ:") {
            // A GraphicEQ line is a *target curve*, not filters — fit it to a
            // parametric bank (shelves + peaks) so the summed response matches.
            // The broadband level folds into the preamp.
            let points = parse_graphic_eq(rest.trim(), ln)?;
            let (preamp_adj, graphic_bands) = crate::graphic::fit_graphic_eq(&points);
            preamp_db += preamp_adj;
            bands.extend(graphic_bands.into_iter().map(|mut b| {
                b.channels = current_channels;
                b
            }));
            continue;
        }
        // Include and unknown directives are silently skipped.
    }

    Ok(Preset {
        name: String::from("APO Import"),
        preamp_db,
        eq_enabled: !bands.is_empty(),
        bands,
        effects: Default::default(),
    })
}

fn parse_filter_line(line: &str, ln: usize, channels: u64) -> Result<Option<EqBand>, ApoError> {
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

    let mut filter_type_str = tokens.next().unwrap_or("").to_string();
    // Shelf slopes are written as two tokens, e.g. "LS 12dB" — fold the slope in.
    if let Some(t) = tokens.peek() {
        if *t == "6dB" || *t == "12dB" {
            filter_type_str.push(' ');
            filter_type_str.push_str(t);
            tokens.next();
        }
    }
    // Unknown filter types (Peace's "None" placeholder, unmodelled Butterworth/
    // Linkwitz-Riley variants, …) skip just this line rather than failing the
    // whole file — matching the line-level leniency for unknown directives.
    let Some(filter_type) = lookup_filter_type(&filter_type_str) else {
        return Ok(None);
    };

    let mut freq = 1000.0f64;
    let mut gain_db = 0.0f64;
    let mut q = 0.707f64;

    // Consume an optional unit token only when it actually is one — otherwise a
    // unit-less "Fc 1000 Gain 3" would swallow the following "Gain" keyword.
    let eat_unit = |tokens: &mut std::iter::Peekable<std::str::SplitWhitespace>, unit: &str| {
        if tokens.peek().is_some_and(|t| t.eq_ignore_ascii_case(unit)) {
            tokens.next();
        }
    };
    while let Some(key) = tokens.next() {
        match key {
            "Fc" => {
                freq = parse_finite(
                    tokens
                        .next()
                        .ok_or_else(|| err(ln, "expected value after Fc"))?,
                    ln,
                    "Fc",
                )?;
                eat_unit(&mut tokens, "Hz");
            }
            "Gain" => {
                gain_db = parse_finite(
                    tokens
                        .next()
                        .ok_or_else(|| err(ln, "expected value after Gain"))?,
                    ln,
                    "Gain",
                )?;
                eat_unit(&mut tokens, "dB");
            }
            "Q" => {
                q = parse_finite(
                    tokens
                        .next()
                        .ok_or_else(|| err(ln, "expected value after Q"))?,
                    ln,
                    "Q",
                )?;
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
        channels,
    }))
}

/// Parse a `Channel:` directive value into a channel-target bitset. `all` (or an
/// empty / wholly-unrecognised list) means every channel. EqualizerAPO allows
/// positions by name (via [`channel_name_to_index`]) OR by 1-based number — both
/// are accepted; numeric is exact for any layout, named tokens assume WAVE order.
fn parse_channel_line(rest: &str) -> u64 {
    let rest = rest.trim();
    if rest.is_empty() || rest.eq_ignore_ascii_case("all") {
        return u64::MAX;
    }
    let mut bits = 0u64;
    for tok in rest.split_whitespace() {
        // A 1-based numeric position takes priority (APO's exact form); else a name.
        let idx = match tok.parse::<usize>() {
            Ok(n) if n >= 1 => Some(n - 1),
            _ => channel_name_to_index(tok),
        };
        if let Some(i) = idx {
            if i < 64 {
                bits |= 1u64 << i;
            }
        }
    }
    // A directive naming only channels we don't model degrades to "all" rather
    // than silently muting every following band.
    if bits == 0 { u64::MAX } else { bits }
}

/// EqualizerAPO channel name → channel index (standard WAVE order). Accepts the
/// common aliases (`FL`/`L`, `LFE`/`SUB`, `BL`/`RL`, …).
fn channel_name_to_index(s: &str) -> Option<usize> {
    Some(match s.trim().to_ascii_uppercase().as_str() {
        "L" | "FL" => 0,
        "R" | "FR" => 1,
        "C" | "FC" => 2,
        "SUB" | "LFE" => 3,
        "RL" | "BL" => 4,
        "RR" | "BR" => 5,
        "SL" => 6,
        "SR" => 7,
        _ => return None,
    })
}

/// Inverse of [`channel_name_to_index`] for the channels APO names (0..8).
fn channel_index_to_name(i: usize) -> Option<&'static str> {
    Some(match i {
        0 => "L",
        1 => "R",
        2 => "C",
        3 => "SUB",
        4 => "RL",
        5 => "RR",
        6 => "SL",
        7 => "SR",
        _ => return None,
    })
}

/// Set channel-bit indices rendered as APO channel tokens. Indices 0..8 use their
/// WAVE names; anything higher (or unnamed) falls back to a 1-based numeric token
/// so every set bit survives the round-trip (APO accepts numeric positions). Scans
/// the full 64-bit range, not just 0..8.
fn channel_bits_to_names(bits: u64) -> Vec<String> {
    (0..64)
        .filter(|&i| bits & (1u64 << i) != 0)
        .map(|i| {
            channel_index_to_name(i)
                .map(str::to_string)
                .unwrap_or_else(|| (i + 1).to_string())
        })
        .collect()
}

/// Serialize a preamp + EQ bands back to EqualizerAPO `.txt` syntax.
///
/// Inverse of [`parse_apo`] for the subset we model (per-band `Filter` lines;
/// the GraphicEQ shorthand is not re-emitted — every band becomes an explicit
/// `Filter` line so the type/Q survive the round-trip).
pub fn write_apo(preamp_db: f64, bands: &[EqBand]) -> String {
    let mut out = String::new();
    out.push_str(&format!("Preamp: {preamp_db:.1} dB\n"));
    // APO scopes filters to the most recent `Channel:` directive (default: all).
    // Emit one only when a band's target differs from what's currently in scope,
    // so all-global band sets round-trip with no `Channel:` lines at all.
    let mut current = u64::MAX;
    for (i, b) in bands.iter().enumerate() {
        if b.channels != current {
            if b.channels == u64::MAX {
                out.push_str("Channel: all\n");
                current = b.channels;
            } else {
                let names = channel_bits_to_names(b.channels);
                // Never emit an empty `Channel:` (a degenerate all-zero mask):
                // APO would re-parse the empty value as `all`. Leave the previous
                // scope in place instead — the numeric fallback means any real
                // (non-zero) mask always yields at least one token.
                if !names.is_empty() {
                    out.push_str(&format!("Channel: {}\n", names.join(" ")));
                    current = b.channels;
                }
            }
        }
        let state = if b.enabled { "ON" } else { "OFF" };
        let kw = apo_keyword(b.filter_type);
        // Shelves/peaking carry gain; pass/notch/allpass omit it (APO ignores it there).
        out.push_str(&format!(
            "Filter {n}: {state} {kw} Fc {fc:.0} Hz",
            n = i + 1,
            fc = b.freq,
        ));
        if filter_uses_gain(b.filter_type) {
            out.push_str(&format!(" Gain {g:.1} dB", g = b.gain_db));
        }
        out.push_str(&format!(" Q {q:.3}\n", q = b.q));
    }
    out
}

/// EqualizerAPO keyword for a filter type (inverse of [`parse_filter_type`]).
fn apo_keyword(t: ApoFilterType) -> &'static str {
    match t {
        ApoFilterType::Peaking => "PK",
        ApoFilterType::LowShelf => "LS",
        ApoFilterType::LowShelf12Db => "LS 12dB",
        ApoFilterType::LowShelfQ => "LSC",
        ApoFilterType::HighShelf => "HS",
        ApoFilterType::HighShelf12Db => "HS 12dB",
        ApoFilterType::HighShelfQ => "HSC",
        ApoFilterType::LowPass => "LP",
        ApoFilterType::LowPassQ => "LPQ",
        ApoFilterType::HighPass => "HP",
        ApoFilterType::HighPassQ => "HPQ",
        ApoFilterType::BandPass => "BP",
        ApoFilterType::Notch => "NO",
        ApoFilterType::AllPass => "AP",
    }
}

/// Whether a filter type carries a meaningful `Gain` term.
fn filter_uses_gain(t: ApoFilterType) -> bool {
    matches!(
        t,
        ApoFilterType::Peaking
            | ApoFilterType::LowShelf
            | ApoFilterType::LowShelf12Db
            | ApoFilterType::LowShelfQ
            | ApoFilterType::HighShelf
            | ApoFilterType::HighShelf12Db
            | ApoFilterType::HighShelfQ
    )
}

/// Map an EqualizerAPO filter keyword to a modelled type, or `None` if the type
/// is unknown/unsupported (inverse of [`apo_keyword`]). A `6dB` shelf slope maps
/// to the maximally-flat (0.707-Q) shelf — the closest slope we model.
fn lookup_filter_type(s: &str) -> Option<ApoFilterType> {
    Some(match s {
        "PK" | "PEQ" => ApoFilterType::Peaking,
        "LS" | "LS 6dB" => ApoFilterType::LowShelf,
        "LS 12dB" => ApoFilterType::LowShelf12Db,
        "LSC" => ApoFilterType::LowShelfQ,
        "HS" | "HS 6dB" => ApoFilterType::HighShelf,
        "HS 12dB" => ApoFilterType::HighShelf12Db,
        "HSC" => ApoFilterType::HighShelfQ,
        "LP" => ApoFilterType::LowPass,
        "LPQ" => ApoFilterType::LowPassQ,
        "HP" => ApoFilterType::HighPass,
        "HPQ" => ApoFilterType::HighPassQ,
        "BP" => ApoFilterType::BandPass,
        "NO" => ApoFilterType::Notch,
        "AP" => ApoFilterType::AllPass,
        _ => return None,
    })
}

/// Parse the `GraphicEQ:` value into `(freq Hz, gain dB)` target points.
fn parse_graphic_eq(s: &str, ln: usize) -> Result<Vec<(f64, f64)>, ApoError> {
    // Format: "20 0; 25 -1.2; 31 0.5; ..." — a freq/gain pair per ';'.
    s.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|pair| {
            let mut parts = pair.split_whitespace();
            let first = parts
                .next()
                .ok_or_else(|| err(ln, "missing freq in GraphicEQ"))?;
            // The gain is normally a separate token, but AutoEq exports often
            // glue the last pair (e.g. "19871-3.0"). When there's no second
            // token, split the freq off at the gain's leading minus sign.
            let (freq_str, gain_str) = match parts.next() {
                Some(g) => (first, g),
                None => {
                    let idx = first
                        .find('-')
                        .ok_or_else(|| err(ln, "missing gain in GraphicEQ"))?;
                    (&first[..idx], &first[idx..])
                }
            };
            let freq = parse_finite(freq_str, ln, "GraphicEQ freq")?;
            let gain_db = parse_finite(gain_str, ln, "GraphicEQ gain")?;
            Ok((freq, gain_db))
        })
        .collect()
}

fn parse_db(s: &str, ln: usize) -> Result<f64, ApoError> {
    let val = s.trim_end_matches("dB").trim();
    parse_finite(val, ln, "dB")
}

/// Parse an `f64` and reject non-finite values. Rust's `f64` parser accepts
/// `"nan"`/`"inf"`, which would poison the DSP chain with NaN/Inf coefficients,
/// so every numeric field from an untrusted preset goes through this.
fn parse_finite(s: &str, ln: usize, what: &str) -> Result<f64, ApoError> {
    let v = s
        .parse::<f64>()
        .map_err(|_| err(ln, &format!("invalid {what} value")))?;
    if !v.is_finite() {
        return Err(err(ln, &format!("{what} value must be finite")));
    }
    Ok(v)
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
    fn parse_graphic_eq_fits_to_bands() {
        // A GraphicEQ target curve is fitted to a parametric bank, not turned
        // into one band per point. (Accuracy is covered in `graphic` tests.)
        let content = "GraphicEQ: 20 -3; 100 -2.5; 1000 0; 10000 1.0\n";
        let preset = parse_apo(content).unwrap();
        assert!(!preset.bands.is_empty());
        assert!(preset.bands.iter().all(|b| b.enabled));
    }

    #[test]
    fn parse_graphic_eq_glued_last_pair() {
        // AutoEq exports often glue the final pair: "19871-3.0" (no space).
        // It must parse without error and produce a fitted band set.
        let content = "GraphicEQ: 20 -8.7; 100 -5.0; 1000 -3.9; 19871-3.0\n";
        let preset = parse_apo(content).unwrap();
        assert!(!preset.bands.is_empty());
    }

    #[test]
    fn write_apo_round_trips_through_parser() {
        let bands = vec![
            EqBand {
                filter_type: ApoFilterType::Peaking,
                freq: 1000.0,
                gain_db: -3.0,
                q: 1.41,
                enabled: true,
                channels: u64::MAX,
            },
            EqBand {
                filter_type: ApoFilterType::HighShelf,
                freq: 10000.0,
                gain_db: 2.5,
                q: 0.707,
                enabled: false,
                channels: u64::MAX,
            },
            EqBand {
                filter_type: ApoFilterType::HighPass,
                freq: 30.0,
                gain_db: 0.0,
                q: 0.707,
                enabled: true,
                channels: u64::MAX,
            },
        ];
        let text = write_apo(-6.0, &bands);
        let re = parse_apo(&text).unwrap();
        assert!((re.preamp_db - (-6.0)).abs() < 1e-9);
        assert_eq!(re.bands.len(), 3);
        for (a, b) in bands.iter().zip(&re.bands) {
            assert_eq!(a.filter_type, b.filter_type, "type mismatch");
            assert!((a.freq - b.freq).abs() < 0.5, "freq mismatch");
            assert!((a.q - b.q).abs() < 0.01, "q mismatch");
            assert_eq!(a.enabled, b.enabled, "enabled mismatch");
            if filter_uses_gain(a.filter_type) {
                assert!((a.gain_db - b.gain_db).abs() < 0.05, "gain mismatch");
            }
        }
    }

    #[test]
    fn channel_directive_scopes_following_filters() {
        // `Channel: L` applies to filters until the next directive; the default
        // (before any directive) is all channels.
        let p = parse_apo(
            "Filter 1: ON PK Fc 100 Hz Gain 1 dB Q 1\n\
             Channel: L\n\
             Filter 2: ON PK Fc 1000 Hz Gain -3 dB Q 1\n\
             Channel: R\n\
             Filter 3: ON PK Fc 2000 Hz Gain 2 dB Q 1\n\
             Channel: all\n\
             Filter 4: ON HS Fc 8000 Hz Gain 1 dB\n",
        )
        .unwrap();
        assert_eq!(p.bands.len(), 4);
        assert_eq!(p.bands[0].channels, u64::MAX, "pre-directive = all");
        assert_eq!(p.bands[1].channels, 0b0001, "L = channel 0");
        assert_eq!(p.bands[2].channels, 0b0010, "R = channel 1");
        assert_eq!(p.bands[3].channels, u64::MAX, "Channel: all");
    }

    #[test]
    fn channel_directive_round_trips_through_writer() {
        let bands = vec![
            EqBand {
                filter_type: ApoFilterType::Peaking,
                freq: 1000.0,
                gain_db: -3.0,
                q: 1.0,
                enabled: true,
                channels: 0b0001, // L only
            },
            EqBand {
                filter_type: ApoFilterType::Peaking,
                freq: 2000.0,
                gain_db: 2.0,
                q: 1.0,
                enabled: true,
                channels: 0b0010, // R only
            },
            EqBand {
                filter_type: ApoFilterType::HighShelf,
                freq: 8000.0,
                gain_db: 1.0,
                q: 0.707,
                enabled: true,
                channels: u64::MAX, // all
            },
        ];
        let text = write_apo(0.0, &bands);
        assert!(
            text.contains("Channel: L\n"),
            "missing L directive:\n{text}"
        );
        assert!(
            text.contains("Channel: R\n"),
            "missing R directive:\n{text}"
        );
        assert!(
            text.contains("Channel: all\n"),
            "missing all directive:\n{text}"
        );

        let re = parse_apo(&text).unwrap();
        assert_eq!(re.bands.len(), 3);
        assert_eq!(re.bands[0].channels, 0b0001);
        assert_eq!(re.bands[1].channels, 0b0010);
        assert_eq!(re.bands[2].channels, u64::MAX);
    }

    #[test]
    fn numeric_channel_directive_parses_one_based() {
        // EqualizerAPO numeric positions are 1-based; `Channel: 1 2` = L+R.
        let p = parse_apo("Channel: 1 2\nFilter 1: ON PK Fc 1000 Hz Gain -3 dB Q 1\n").unwrap();
        assert_eq!(p.bands[0].channels, 0b11);
        let p2 = parse_apo("Channel: 1\nFilter 1: ON PK Fc 1000 Hz Gain -3 dB Q 1\n").unwrap();
        assert_eq!(p2.bands[0].channels, 0b01);
        // Mixed numeric + name: `2 C` = R (idx1) + C (idx2).
        let p3 = parse_apo("Channel: 2 C\nFilter 1: ON PK Fc 1000 Hz Gain 1 dB Q 1\n").unwrap();
        assert_eq!(p3.bands[0].channels, 0b110);
    }

    #[test]
    fn high_channel_band_round_trips_via_numeric() {
        // A band on channel 8 (no WAVE name) must survive write→parse via the
        // numeric fallback rather than collapsing to ALL or dropping the bit.
        let bands = vec![EqBand {
            filter_type: ApoFilterType::Peaking,
            freq: 1000.0,
            gain_db: 2.0,
            q: 1.0,
            enabled: true,
            channels: 1u64 << 8,
        }];
        let text = write_apo(0.0, &bands);
        assert!(
            text.contains("Channel: 9"),
            "expected numeric token:\n{text}"
        );
        let re = parse_apo(&text).unwrap();
        assert_eq!(re.bands[0].channels, 1u64 << 8);
    }

    #[test]
    fn all_global_bands_emit_no_channel_directive() {
        // The common case (every band global) must not sprout `Channel:` lines.
        let bands = vec![EqBand {
            filter_type: ApoFilterType::Peaking,
            freq: 1000.0,
            gain_db: 3.0,
            q: 1.0,
            enabled: true,
            channels: u64::MAX,
        }];
        let text = write_apo(0.0, &bands);
        assert!(!text.contains("Channel:"), "unexpected directive:\n{text}");
    }

    #[test]
    fn rejects_non_finite_values() {
        // Hostile presets must not feed NaN/Inf into the DSP chain (a NaN gain
        // in a GraphicEQ line used to panic the curve fitter).
        assert!(parse_apo("GraphicEQ: 20 nan; 100 2; 1000 0; 5000 1\n").is_err());
        assert!(parse_apo("GraphicEQ: 20 0; inf 2; 1000 0\n").is_err());
        assert!(parse_apo("Filter 1: ON PK Fc nan Hz Gain 3 dB Q 1\n").is_err());
        assert!(parse_apo("Filter 1: ON PK Fc 1000 Hz Gain inf dB Q 1\n").is_err());
        assert!(parse_apo("Preamp: nan dB\n").is_err());
    }

    #[test]
    fn six_db_shelf_slope_parses() {
        let p = parse_apo("Filter 1: ON LS 6dB Fc 100 Hz Gain 3 dB\n").unwrap();
        assert_eq!(p.bands.len(), 1);
        assert_eq!(p.bands[0].filter_type, ApoFilterType::LowShelf);
        let p = parse_apo("Filter 1: ON HS 6dB Fc 8000 Hz Gain -2 dB\n").unwrap();
        assert_eq!(p.bands[0].filter_type, ApoFilterType::HighShelf);
    }

    #[test]
    fn unknown_filter_type_skips_only_that_line() {
        // Peace writes "ON None" placeholders and APO has types we don't model
        // (BWLP/LRHP/…); one unknown keyword must not reject the whole file.
        let p = parse_apo(
            "Filter 1: ON None\nFilter 2: ON BWLP Fc 80 Hz\nFilter 3: ON PK Fc 1000 Hz Gain -3 dB Q 1\n",
        )
        .unwrap();
        assert_eq!(p.bands.len(), 1);
        assert_eq!(p.bands[0].filter_type, ApoFilterType::Peaking);
    }

    #[test]
    fn missing_unit_token_does_not_swallow_next_keyword() {
        // "Fc 1000 Gain 3" (no "Hz" unit) must still read the Gain.
        let p = parse_apo("Filter 1: ON PK Fc 1000 Gain 3 Q 2\n").unwrap();
        assert_eq!(p.bands.len(), 1);
        assert!((p.bands[0].freq - 1000.0).abs() < 0.01);
        assert!((p.bands[0].gain_db - 3.0).abs() < 0.01);
        assert!((p.bands[0].q - 2.0).abs() < 0.01);
    }

    #[test]
    fn write_apo_emits_all_keywords() {
        use ApoFilterType::*;
        for t in [
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
        ] {
            let band = EqBand {
                filter_type: t,
                freq: 500.0,
                gain_db: 1.0,
                q: 1.0,
                enabled: true,
                channels: u64::MAX,
            };
            let text = write_apo(0.0, &[band]);
            let re = parse_apo(&text).unwrap();
            assert_eq!(re.bands[0].filter_type, t, "keyword round-trip for {t:?}");
        }
    }
}
