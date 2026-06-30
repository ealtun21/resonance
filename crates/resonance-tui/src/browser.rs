//! Minimal file picker for loading presets, with a live preview pane that
//! parses `.fac` / APO `.txt` files into a readable summary.

use resonance_dsp::filter::FilterType;
use resonance_ipc::BandType;
use resonance_preset::{apo::parse_apo, fac::parse_fac};
use std::path::{Path, PathBuf};

pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Why the picker is open — decides what happens when a file is chosen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BrowsePurpose {
    /// Import + load a preset/profile (`.fac` / APO `.txt` / `.toml`).
    LoadPreset,
    /// Load a measurement curve into the reference overlay (`.txt` / `.csv`).
    LoadMeasurement,
}

pub struct Browser {
    pub cwd: PathBuf,
    pub entries: Vec<Item>,
    pub cursor: usize,
    /// Preview lines for the currently selected entry.
    pub preview: Vec<String>,
    pub purpose: BrowsePurpose,
}

impl Browser {
    pub fn new(start: PathBuf) -> Self {
        Self::with_purpose(start, BrowsePurpose::LoadPreset)
    }

    /// A picker for loading a measurement curve into the reference overlay.
    pub fn new_measurement(start: PathBuf) -> Self {
        Self::with_purpose(start, BrowsePurpose::LoadMeasurement)
    }

    fn with_purpose(start: PathBuf, purpose: BrowsePurpose) -> Self {
        let cwd = if start.is_dir() {
            start
        } else {
            PathBuf::from(".")
        };
        let mut b = Self {
            cwd,
            entries: Vec::new(),
            cursor: 0,
            preview: Vec::new(),
            purpose,
        };
        b.reload();
        b
    }

    fn reload(&mut self) {
        self.entries = read_entries(&self.cwd, self.purpose);
        self.cursor = 0;
        self.update_preview();
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as i32 - 1;
        let next = (self.cursor as i32 + delta).clamp(0, max);
        self.cursor = next as usize;
        self.update_preview();
    }

    /// Go to the parent directory.
    pub fn parent(&mut self) {
        if let Some(p) = self.cwd.parent() {
            self.cwd = p.to_path_buf();
            self.reload();
        }
    }

    /// Activate the selected entry. Returns `Some(path)` if it is a preset file
    /// to load, or `None` if it was a directory (now navigated into).
    pub fn enter(&mut self) -> Option<String> {
        let item = self.entries.get(self.cursor)?;
        if item.is_dir {
            self.cwd = item.path.clone();
            self.reload();
            None
        } else {
            Some(item.path.to_string_lossy().into_owned())
        }
    }

    fn update_preview(&mut self) {
        self.preview = match self.entries.get(self.cursor) {
            Some(it) if !it.is_dir => preview_file(&it.path),
            Some(it) => vec![format!("📁 {}/", it.name)],
            None => vec!["(empty directory)".to_string()],
        };
    }
}

/// Directory listing: `..` (if any), then sub-directories, then the files this
/// picker accepts (preset vs measurement extensions). Hidden entries skipped.
/// Each group sorted case-insensitively.
fn read_entries(dir: &Path, purpose: BrowsePurpose) -> Vec<Item> {
    let mut dirs: Vec<Item> = Vec::new();
    let mut files: Vec<Item> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                dirs.push(Item { name, path, is_dir });
            } else if accepts(&name, purpose) {
                files.push(Item { name, path, is_dir });
            }
        }
    }

    let key = |i: &Item| i.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);

    let mut out = Vec::with_capacity(dirs.len() + files.len() + 1);
    if let Some(parent) = dir.parent() {
        out.push(Item {
            name: "..".to_string(),
            path: parent.to_path_buf(),
            is_dir: true,
        });
    }
    out.extend(dirs);
    out.extend(files);
    out
}

/// Which files a picker lists, by purpose: presets are `.fac`/APO `.txt`/
/// `.toml`; measurement curves are `.txt`/`.csv`. Keeping these separate stops
/// the preset picker from offering `.csv` curves (which the importer rejects)
/// and the measurement picker from offering `.fac`/`.toml` (which it can't parse).
fn accepts(name: &str, purpose: BrowsePurpose) -> bool {
    let has_ext = |ext: &str| {
        Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    };
    match purpose {
        BrowsePurpose::LoadPreset => has_ext("fac") || has_ext("txt") || has_ext("toml"),
        BrowsePurpose::LoadMeasurement => has_ext("txt") || has_ext("csv"),
    }
}

/// Read at most `max` bytes of a file as lossy UTF-8 (for bounded previews).
fn read_head(path: &Path, max: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(max as u64)
        .read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Build a human-readable preview for a preset file (falls back to a raw head).
fn preview_file(path: &Path) -> Vec<String> {
    const MAX_BANDS: usize = 16;
    // Cap the read: this runs on every cursor move while browsing, so a huge or
    // hostile file shouldn't be slurped whole. Real presets are a few KB; 64 KiB
    // is plenty to summarise, and the parsers tolerate a truncated tail.
    let Ok(content) = read_head(path, 64 * 1024) else {
        return vec!["(cannot read file)".to_string()];
    };

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    // `.toml` profiles aren't preset files — the daemon owns their schema. Show
    // a raw head instead of running them through the preset parsers.
    if ext == "toml" {
        let mut out = vec!["⮞ profile (.toml)".to_string(), String::new()];
        out.extend(
            content
                .lines()
                .take(30)
                .map(std::string::ToString::to_string),
        );
        return out;
    }
    // GraphicEQ target curves are fitted to a parametric bank on import — an
    // expensive optimisation. Don't run it for a hover preview; summarise the
    // raw target instead.
    if let Some(summary) = graphic_eq_preview(&content) {
        return summary;
    }
    let parsed = if ext == "fac" {
        parse_fac(&content).map_err(|e| e.to_string())
    } else {
        parse_apo(&content).map_err(|e| e.to_string())
    };

    match parsed {
        Ok(p) => {
            let mut out = Vec::new();
            out.push(format!(
                "⮞ {}",
                if p.name.is_empty() {
                    "(unnamed)"
                } else {
                    &p.name
                }
            ));
            out.push(format!("preamp {:+.1} dB", p.preamp_db));
            out.push(String::new());
            out.push("Effects".to_string());
            let fx = &p.effects;
            for (label, st) in [
                ("Fidelity ", &fx.fidelity),
                ("Ambience ", &fx.ambience),
                ("Surround ", &fx.surround),
                ("DynBoost ", &fx.dynamic_boost),
                ("Bass     ", &fx.bass),
            ] {
                let mark = if st.enabled { "●" } else { "○" };
                out.push(format!(
                    " {mark} {label} {:>3}%",
                    (st.intensity * 100.0).round() as i32
                ));
            }
            out.push(String::new());
            out.push(format!("EQ bands: {}", p.bands.len()));
            for b in p.bands.iter().take(MAX_BANDS) {
                let bt: BandType = FilterType::from(b.filter_type).into();
                out.push(format!(
                    " {:2} {:>7} {:+5.1}dB Q{:.2}",
                    bt.abbrev(),
                    fmt_freq(b.freq),
                    b.gain_db,
                    b.q
                ));
            }
            if p.bands.len() > MAX_BANDS {
                out.push(format!(" … (+{} more)", p.bands.len() - MAX_BANDS));
            }
            out
        }
        Err(e) => {
            let mut out = vec![format!("(parse error: {e})"), String::new()];
            out.extend(
                content
                    .lines()
                    .take(30)
                    .map(std::string::ToString::to_string),
            );
            out
        }
    }
}

fn fmt_freq(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{:.1}kHz", hz / 1000.0)
    } else {
        format!("{hz:.0}Hz")
    }
}

/// Cheap preview for an `EqualizerAPO` `GraphicEQ:` target line — point count and
/// range, without running the (expensive) curve-fit that import performs.
fn graphic_eq_preview(content: &str) -> Option<Vec<String>> {
    let s = resonance_preset::graphic::graphic_eq_summary(content)?;
    Some(vec![
        "⮞ GraphicEQ target curve".to_string(),
        format!(
            "{} points  {}–{}",
            s.points,
            fmt_freq(s.min_hz),
            fmt_freq(s.max_hz)
        ),
        format!("gain {:+.1} … {:+.1} dB", s.min_gain, s.max_gain),
        String::new(),
        "Fitted to parametric bands".to_string(),
        "(shelves + peaks) on import.".to_string(),
    ])
}
