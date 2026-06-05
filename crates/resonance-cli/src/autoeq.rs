//! AutoEq integration: download a headphone correction from the community
//! database and drop it into the XDG preset library.
//!
//! The AutoEq `results/INDEX.md` lists every measurement as
//! `- [Display Name](./source/form/Model) by Source`, where the link is the
//! (URL-encoded) result directory. The parametric correction lives in that dir
//! as `<Model> ParametricEQ.txt` — already in EqualizerAPO syntax, which our
//! parser reads directly.

use anyhow::{Result, bail};
use std::path::PathBuf;

const RESULTS_BASE: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results";

/// One parsed INDEX.md entry. Paths stay URL-encoded for building download URLs;
/// `display`/`source` are decoded for matching and naming.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub display: String,
    pub source: String,
    /// Result dir relative to `results/`, URL-encoded (e.g. `oratory1990/over-ear/Sennheiser%20HD%20600`).
    pub path_enc: String,
    /// Last path segment, URL-encoded (the model dir name).
    pub model_enc: String,
}

/// Download + import: returns the local file path written into the preset library.
pub fn run(query: &str) -> Result<PathBuf> {
    let index = http_get(&format!("{RESULTS_BASE}/INDEX.md"))?;
    let entries = parse_index(&index);
    let mut matches = find_matches(&entries, query);
    if matches.is_empty() {
        bail!("no AutoEq result matches '{query}'");
    }
    rank(&mut matches);
    let best = matches[0].clone();
    eprintln!(
        "AutoEq: {} by {} ({} match{})",
        best.display,
        best.source,
        matches.len(),
        if matches.len() == 1 { "" } else { "es" }
    );

    let url = format!(
        "{RESULTS_BASE}/{}/{}%20ParametricEQ.txt",
        best.path_enc, best.model_enc
    );
    let body = http_get(&url)?;

    let dir = resonance_ipc::paths::user_preset_dir();
    std::fs::create_dir_all(&dir)?;
    let fname = sanitize_filename(&format!("{} ({}).txt", best.display, best.source));
    let path = dir.join(fname);
    std::fs::write(&path, body)?;
    Ok(path)
}

fn http_get(url: &str) -> Result<String> {
    let body = ureq::get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| anyhow::anyhow!("read {url}: {e}"))?;
    Ok(body)
}

fn parse_index(index: &str) -> Vec<Entry> {
    index.lines().filter_map(parse_line).collect()
}

/// Parse one `- [Display](./path) by Source` line.
fn parse_line(line: &str) -> Option<Entry> {
    let line = line.trim().strip_prefix("- ")?;
    let line = line.strip_prefix('[')?;
    let close = line.find("](")?;
    let display = decode(&line[..close]);
    let rest = &line[close + 2..];
    let end = rest.find(')')?;
    let path_enc = rest[..end].trim_start_matches("./").to_string();
    if path_enc.is_empty() {
        return None;
    }
    let source = rest[end + 1..]
        .trim()
        .strip_prefix("by ")
        .unwrap_or("")
        .trim()
        .to_string();
    let model_enc = path_enc.rsplit('/').next()?.to_string();
    Some(Entry {
        display,
        source,
        path_enc,
        model_enc,
    })
}

/// Case/space/punctuation-insensitive substring key for matching.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn find_matches(entries: &[Entry], query: &str) -> Vec<Entry> {
    let q = norm(query);
    if q.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|e| norm(&e.display).contains(&q))
        .cloned()
        .collect()
}

/// Prefer high-quality measurement sources, then shorter (closer) names.
fn rank(matches: &mut [Entry]) {
    fn source_rank(src: &str) -> u8 {
        match src.to_ascii_lowercase().as_str() {
            "oratory1990" => 0,
            "crinacle" => 1,
            "rtings" => 2,
            _ => 3,
        }
    }
    matches.sort_by(|a, b| {
        source_rank(&a.source)
            .cmp(&source_rank(&b.source))
            .then(a.display.len().cmp(&b.display.len()))
            .then(a.display.cmp(&b.display))
    });
}

/// Minimal percent-decoding (UTF-8 aware) for INDEX paths/names.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Strip path separators and control chars from a profile/file name.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Index
- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990
- [Sennheiser HD 600](./crinacle/GRAS%2043AG-7%20over-ear/Sennheiser%20HD%20600) by crinacle on GRAS 43AG-7
- [AKG K371](./Super%20Review/over-ear/AKG%20K371) by Super Review
not an entry line
";

    #[test]
    fn parses_index_entries() {
        let entries = parse_index(SAMPLE);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].display, "Sennheiser HD 600");
        assert_eq!(entries[0].source, "oratory1990");
        assert_eq!(
            entries[0].path_enc,
            "oratory1990/over-ear/Sennheiser%20HD%20600"
        );
        assert_eq!(entries[0].model_enc, "Sennheiser%20HD%20600");
    }

    #[test]
    fn matching_is_space_and_case_insensitive() {
        let entries = parse_index(SAMPLE);
        let m = find_matches(&entries, "hd600");
        assert_eq!(m.len(), 2, "both HD 600 sources should match 'hd600'");
        let m = find_matches(&entries, "k371");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].display, "AKG K371");
    }

    #[test]
    fn rank_prefers_oratory1990() {
        let entries = parse_index(SAMPLE);
        let mut m = find_matches(&entries, "hd 600");
        rank(&mut m);
        assert_eq!(m[0].source, "oratory1990");
    }

    #[test]
    fn decode_handles_spaces_and_utf8() {
        assert_eq!(decode("Sennheiser%20HD%20600"), "Sennheiser HD 600");
        assert_eq!(decode("GRAS%2043AG-7"), "GRAS 43AG-7");
        assert_eq!(decode("plain"), "plain");
    }
}
