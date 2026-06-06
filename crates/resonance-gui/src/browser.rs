//! Minimal preset file picker with a parsed preview (shared logic with the TUI).

use resonance_dsp::filter::FilterType;
use resonance_ipc::BandType;
use resonance_preset::{apo::parse_apo, fac::parse_fac};
use std::path::{Path, PathBuf};

pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub struct Browser {
    pub cwd: PathBuf,
    pub entries: Vec<Item>,
    pub cursor: usize,
    pub preview: Vec<String>,
}

impl Browser {
    pub fn new(start: PathBuf) -> Self {
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
        };
        b.reload();
        b
    }

    fn reload(&mut self) {
        self.entries = read_entries(&self.cwd);
        self.cursor = 0;
        self.update_preview();
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.entries.len() {
            self.cursor = idx;
            self.update_preview();
        }
    }

    pub fn parent(&mut self) {
        if let Some(p) = self.cwd.parent() {
            self.cwd = p.to_path_buf();
            self.reload();
        }
    }

    /// Activate entry at `idx`: navigate into a directory (returns `None`) or
    /// return the preset path to load.
    pub fn activate(&mut self, idx: usize) -> Option<String> {
        let item = self.entries.get(idx)?;
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
            Some(it) => vec![format!("{}/", it.name)],
            None => vec!["(empty directory)".to_string()],
        };
    }
}

fn read_entries(dir: &Path) -> Vec<Item> {
    let mut dirs: Vec<Item> = Vec::new();
    let mut files: Vec<Item> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                dirs.push(Item { name, path, is_dir });
            } else if is_preset(&name) {
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

fn is_preset(name: &str) -> bool {
    name.ends_with(".fac") || name.ends_with(".txt") || name.ends_with(".toml")
}

/// Our own `.toml` export format (mirror of the daemon's `Profile`), used here
/// only to render a preview of a native profile file.
#[derive(serde::Deserialize)]
struct ProfilePreview {
    #[serde(default)]
    preamp_db: f64,
    #[serde(default)]
    bands: Vec<resonance_ipc::BandState>,
}

fn preview_file(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec!["(cannot read file)".to_string()];
    };

    // Native `.toml` profiles render their own compact summary.
    if path.extension().map(|e| e == "toml").unwrap_or(false) {
        return match toml::from_str::<ProfilePreview>(&content) {
            Ok(p) => {
                let mut out = vec![
                    "Resonance profile".to_string(),
                    format!("preamp {:+.1} dB", p.preamp_db),
                    String::new(),
                    format!("EQ bands: {}", p.bands.len()),
                ];
                for b in p.bands.iter().take(16) {
                    out.push(format!(
                        " {:2} {:>7} {:+5.1}dB Q{:.2}",
                        b.band_type.abbrev(),
                        fmt_freq(b.freq),
                        b.gain_db,
                        b.q
                    ));
                }
                out
            }
            Err(e) => vec![format!("(parse error: {e})")],
        };
    }

    let is_fac = path.extension().map(|e| e == "fac").unwrap_or(false);
    let parsed = if is_fac {
        parse_fac(&content).map_err(|e| e.to_string())
    } else {
        parse_apo(&content).map_err(|e| e.to_string())
    };

    match parsed {
        Ok(p) => {
            let mut out = Vec::new();
            out.push(
                if p.name.is_empty() {
                    "(unnamed)"
                } else {
                    &p.name
                }
                .to_string(),
            );
            out.push(format!("preamp {:+.1} dB", p.preamp_db));
            out.push(String::new());
            out.push("Effects".to_string());
            let fx = &p.effects;
            for (label, st) in [
                ("Fidelity", &fx.fidelity),
                ("Ambience", &fx.ambience),
                ("Surround", &fx.surround),
                ("DynBoost", &fx.dynamic_boost),
                ("Bass", &fx.bass),
            ] {
                let mark = if st.enabled { "on " } else { "off" };
                out.push(format!(
                    " {mark} {label:<9} {:>3}%",
                    (st.intensity * 100.0).round() as i32
                ));
            }
            out.push(String::new());
            out.push(format!("EQ bands: {}", p.bands.len()));
            const MAX_BANDS: usize = 16;
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
            out.extend(content.lines().take(30).map(|l| l.to_string()));
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
