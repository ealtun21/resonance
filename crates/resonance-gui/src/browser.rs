//! Filesystem navigator backing the Load / Export dialogs, with a parsed
//! preview of the highlighted preset. Shared preview logic mirrors the TUI.

use resonance_dsp::filter::FilterType;
use resonance_ipc::BandType;
use resonance_preset::{apo::parse_apo, fac::parse_fac};
use std::path::{Path, PathBuf};

pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// File this navigator considers loadable (`.fac` / `.txt` / `.toml`).
    pub is_preset: bool,
}

pub struct Browser {
    pub cwd: PathBuf,
    /// Unfiltered directory contents (no synthetic `..`).
    all: Vec<Item>,
    /// Visible rows after the search filter, with `..` prepended.
    pub entries: Vec<Item>,
    pub cursor: usize,
    pub preview: Vec<String>,
    /// Case-insensitive name filter from the search box.
    pub filter: String,
    /// Editable location-bar buffer (kept in sync with `cwd` on navigation).
    pub path_edit: String,
    /// Save dialogs list every file (so existing names show); load dialogs
    /// list only loadable presets.
    show_non_presets: bool,
}

impl Browser {
    pub fn new(start: PathBuf, show_non_presets: bool) -> Self {
        let cwd = first_existing_dir(start);
        let mut b = Self {
            path_edit: cwd.display().to_string(),
            cwd,
            all: Vec::new(),
            entries: Vec::new(),
            cursor: 0,
            preview: Vec::new(),
            filter: String::new(),
            show_non_presets,
        };
        b.reload();
        b
    }

    fn reload(&mut self) {
        self.all = read_entries(&self.cwd, self.show_non_presets);
        self.path_edit = self.cwd.display().to_string();
        self.cursor = 0;
        self.refilter();
    }

    /// Rebuild `entries` from `all` applying the current name filter, always
    /// keeping `..` (when a parent exists) at the top.
    pub fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        let mut out: Vec<Item> = Vec::new();
        if let Some(parent) = self.cwd.parent() {
            out.push(Item {
                name: "..".to_string(),
                path: parent.to_path_buf(),
                is_dir: true,
                is_preset: false,
            });
        }
        for it in &self.all {
            if f.is_empty() || it.name.to_lowercase().contains(&f) {
                out.push(Item {
                    name: it.name.clone(),
                    path: it.path.clone(),
                    is_dir: it.is_dir,
                    is_preset: it.is_preset,
                });
            }
        }
        self.entries = out;
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
        self.update_preview();
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.entries.len() {
            self.cursor = idx;
            self.update_preview();
        }
    }

    /// Move the highlight by `delta`, clamped to the list bounds.
    pub fn move_cursor(&mut self, delta: i64) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as i64 - 1;
        let next = (self.cursor as i64 + delta).clamp(0, last) as usize;
        self.select(next);
    }

    pub fn parent(&mut self) {
        if let Some(p) = self.cwd.parent() {
            self.navigate(p.to_path_buf());
        }
    }

    /// Change directory (no-op for non-directories).
    pub fn navigate(&mut self, dir: PathBuf) {
        if dir.is_dir() {
            self.cwd = dir;
            self.filter.clear();
            self.reload();
        }
    }

    /// Apply the typed location bar: cd into a directory, or load a file.
    pub fn go_to_typed(&mut self) -> Option<String> {
        let p = PathBuf::from(shellexpand_tilde(&self.path_edit));
        if p.is_dir() {
            self.navigate(p);
            None
        } else if p.is_file() {
            Some(p.to_string_lossy().into_owned())
        } else {
            // Bad path: snap the buffer back to the current directory.
            self.path_edit = self.cwd.display().to_string();
            None
        }
    }

    /// Activate entry `idx`: descend into a directory (returns `None`) or yield
    /// the file path to load.
    pub fn activate(&mut self, idx: usize) -> Option<String> {
        let (is_dir, path) = {
            let item = self.entries.get(idx)?;
            (item.is_dir, item.path.clone())
        };
        if is_dir {
            self.navigate(path);
            None
        } else {
            Some(path.to_string_lossy().into_owned())
        }
    }

    /// The currently highlighted entry, if any.
    pub fn selected(&self) -> Option<&Item> {
        self.entries.get(self.cursor)
    }

    fn update_preview(&mut self) {
        self.preview = match self.entries.get(self.cursor) {
            Some(it) if !it.is_dir => preview_file(&it.path),
            Some(it) if it.name == ".." => vec!["↑ parent directory".to_string()],
            Some(it) => vec![format!("▸ {}/", it.name)],
            None => vec!["(nothing selected)".to_string()],
        };
    }
}

/// First existing directory at or above `start` (files resolve to their dir).
fn first_existing_dir(start: PathBuf) -> PathBuf {
    let mut p = if start.is_file() {
        start.parent().map(Path::to_path_buf).unwrap_or(start)
    } else {
        start
    };
    while !p.is_dir() {
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => return home_dir(),
        }
    }
    p
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

/// Expand a leading `~` to `$HOME` (only the simple `~` / `~/...` forms).
fn shellexpand_tilde(s: &str) -> String {
    let s = s.trim();
    if s == "~" {
        home_dir().display().to_string()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home_dir().join(rest).display().to_string()
    } else {
        s.to_string()
    }
}

fn read_entries(dir: &Path, show_non_presets: bool) -> Vec<Item> {
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
                dirs.push(Item {
                    name,
                    path,
                    is_dir,
                    is_preset: false,
                });
            } else {
                let preset = is_preset(&name);
                if preset || show_non_presets {
                    files.push(Item {
                        name,
                        path,
                        is_dir,
                        is_preset: preset,
                    });
                }
            }
        }
    }

    let key = |i: &Item| i.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);

    let mut out = Vec::with_capacity(dirs.len() + files.len());
    out.extend(dirs);
    out.extend(files);
    out
}

pub fn is_preset(name: &str) -> bool {
    let n = name.to_lowercase();
    n.ends_with(".fac") || n.ends_with(".txt") || n.ends_with(".toml")
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

    // GraphicEQ target curves are fitted to a parametric bank on import — an
    // expensive optimisation. Don't run it for a hover preview; summarise the
    // raw target instead.
    if let Some(summary) = graphic_eq_preview(&content) {
        return summary;
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

/// Cheap preview for an EqualizerAPO `GraphicEQ:` target line — point count and
/// range, without running the (expensive) curve-fit that import performs.
fn graphic_eq_preview(content: &str) -> Option<Vec<String>> {
    let s = resonance_preset::graphic::graphic_eq_summary(content)?;
    Some(vec![
        "GraphicEQ target curve".to_string(),
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
