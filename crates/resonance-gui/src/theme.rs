//! Colour themes for the GUI.
//!
//! A [`Theme`] maps to two things: an [`egui::Visuals`] (window/panel/widget
//! chrome) and a [`Palette`] of semantic colours the custom painters use (EQ
//! curve accent, gain boost/cut tints, graph background, grid). Keeping the
//! palette separate from `Visuals` lets the response curve and gain bars stay
//! theme-aware without reaching into widget internals.

use eframe::egui::{self, Color32};

/// Semantic colours used by the custom EQ/spectrum painters.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Curve line base, band markers, selection.
    pub accent: Color32,
    /// Positive-gain tint (boost).
    pub boost: Color32,
    /// Negative-gain tint (cut).
    pub cut: Color32,
    /// Near-zero gain / idle baseline.
    pub neutral: Color32,
    /// Plot background (matches `Visuals::extreme_bg_color`).
    pub graph_bg: Color32,
    /// Grid lines.
    pub grid: Color32,
    /// Selected/locked band highlight.
    pub highlight: Color32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    BreezeDark,
    Gruvbox,
    Nord,
    MatrixGreen,
    Light,
    /// Material-you palette loaded from a matugen/pywal colours file.
    Matugen,
}

impl Theme {
    /// Every selectable theme, in menu order.
    pub const ALL: [Theme; 7] = [
        Theme::System,
        Theme::BreezeDark,
        Theme::Gruvbox,
        Theme::Nord,
        Theme::MatrixGreen,
        Theme::Light,
        Theme::Matugen,
    ];

    /// Resolve a theme from its [`label`](Self::label); used to restore the
    /// persisted choice across runs. Unknown labels fall back to `System`.
    pub fn from_label(s: &str) -> Theme {
        Theme::ALL
            .into_iter()
            .find(|t| t.label() == s)
            .unwrap_or(Theme::System)
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System dark",
            Theme::BreezeDark => "Breeze Dark",
            Theme::Gruvbox => "Gruvbox",
            Theme::Nord => "Nord",
            Theme::MatrixGreen => "Matrix",
            Theme::Light => "Light",
            Theme::Matugen => "Matugen (auto)",
        }
    }

    /// Hard-coded palette for this theme (Matugen falls back to dark if no file).
    pub fn palette(self) -> Palette {
        match self {
            Theme::System => Palette {
                accent: rgb(80, 200, 255),
                boost: rgb(70, 200, 90),
                cut: rgb(225, 80, 80),
                neutral: rgb(150, 150, 160),
                graph_bg: rgb(20, 22, 26),
                grid: rgb(70, 74, 82),
                highlight: Color32::YELLOW,
            },
            Theme::BreezeDark => Palette {
                accent: rgb(61, 174, 233), // breeze blue
                boost: rgb(39, 174, 96),
                cut: rgb(218, 68, 83),
                neutral: rgb(127, 140, 141),
                graph_bg: rgb(27, 30, 32),
                grid: rgb(61, 67, 71),
                highlight: rgb(246, 116, 0), // breeze orange
            },
            Theme::Gruvbox => Palette {
                accent: rgb(131, 165, 152), // aqua
                boost: rgb(152, 151, 26),   // green
                cut: rgb(204, 36, 29),      // red
                neutral: rgb(168, 153, 132),
                graph_bg: rgb(40, 40, 40),
                grid: rgb(80, 73, 69),
                highlight: rgb(250, 189, 47), // yellow
            },
            Theme::Nord => Palette {
                accent: rgb(136, 192, 208), // nord8
                boost: rgb(163, 190, 140),  // nord14
                cut: rgb(191, 97, 106),     // nord11
                neutral: rgb(143, 156, 179),
                graph_bg: rgb(46, 52, 64),     // nord0
                grid: rgb(67, 76, 94),         // nord1/2
                highlight: rgb(235, 203, 139), // nord13
            },
            Theme::MatrixGreen => Palette {
                accent: rgb(0, 255, 70),
                boost: rgb(0, 230, 80),
                cut: rgb(0, 120, 40),
                neutral: rgb(0, 110, 40),
                graph_bg: rgb(0, 8, 0),
                grid: rgb(0, 60, 20),
                highlight: rgb(180, 255, 120),
            },
            Theme::Light => Palette {
                accent: rgb(20, 120, 200),
                boost: rgb(30, 150, 60),
                cut: rgb(200, 50, 50),
                neutral: rgb(110, 110, 120),
                graph_bg: rgb(245, 246, 248),
                grid: rgb(200, 204, 210),
                highlight: rgb(220, 130, 0),
            },
            Theme::Matugen => matugen_palette().unwrap_or_else(|| Theme::System.palette()),
        }
    }

    /// Whether this theme uses a light base (affects text contrast defaults).
    fn is_light(self) -> bool {
        match self {
            Theme::Light => true,
            Theme::Matugen => matugen_is_light(),
            _ => false,
        }
    }

    /// Build the full `egui::Visuals` for this theme.
    pub fn visuals(self) -> egui::Visuals {
        let p = self.palette();
        let mut v = if self.is_light() {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        let panel = darken(p.graph_bg, 1.18);
        v.panel_fill = panel;
        v.window_fill = panel;
        v.extreme_bg_color = p.graph_bg;
        v.faint_bg_color = lighten(panel, 1.12);
        v.window_stroke = egui::Stroke::new(1.0, p.grid);
        v.selection.bg_fill = p.accent.gamma_multiply(0.55);
        v.selection.stroke = egui::Stroke::new(1.0, p.accent);
        v.hyperlink_color = p.accent;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.accent);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, p.accent);
        v
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn darken(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 / f) as u8,
        (c.g() as f32 / f) as u8,
        (c.b() as f32 / f) as u8,
    )
}

fn lighten(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f).min(255.0) as u8,
        (c.g() as f32 * f).min(255.0) as u8,
        (c.b() as f32 * f).min(255.0) as u8,
    )
}

// ── matugen / pywal palette loading ─────────────────────────────────────────

/// Candidate colour files, newest-style first. pywal and most matugen templates
/// write a JSON document with `special.background/foreground` and `colors.colorN`.
fn matugen_files() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(cfg) = std::env::var("XDG_CONFIG_HOME") {
        v.push(std::path::PathBuf::from(cfg).join("resonance/colors.json"));
    }
    if let Ok(home) = std::env::var("HOME") {
        let h = std::path::PathBuf::from(home);
        v.push(h.join(".config/resonance/colors.json"));
        v.push(h.join(".cache/wal/colors.json"));
    }
    v
}

/// Extremely small JSON colour extractor: pulls `"key": "#rrggbb"` pairs without
/// pulling in a JSON dependency. Returns the first existing file's map.
fn load_color_map() -> Option<std::collections::HashMap<String, Color32>> {
    let path = matugen_files().into_iter().find(|p| p.is_file())?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut map = std::collections::HashMap::new();
    // Match `"<key>": "#rrggbb"` occurrences.
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(hash) = text[i..].find('#') {
        let start = i + hash;
        // hex digits after '#'
        let mut j = start + 1;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        if j - start >= 7 {
            if let Some(c) = parse_hex(&text[start..start + 7]) {
                // Back-scan for the preceding quoted key.
                if let Some(key) = preceding_key(&text[..start]) {
                    map.entry(key).or_insert(c);
                }
            }
        }
        i = j.max(start + 1);
    }
    if map.is_empty() { None } else { Some(map) }
}

/// Find the JSON object key (`"name":`) immediately before `before`.
fn preceding_key(before: &str) -> Option<String> {
    let colon = before.rfind(':')?;
    let head = &before[..colon];
    let close = head.rfind('"')?;
    let open = head[..close].rfind('"')?;
    Some(head[open + 1..close].to_string())
}

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#')?;
    if s.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

fn matugen_palette() -> Option<Palette> {
    let m = load_color_map()?;
    let bg = m
        .get("background")
        .or_else(|| m.get("color0"))
        .copied()
        .unwrap_or(rgb(20, 22, 26));
    let accent = m
        .get("color4")
        .or_else(|| m.get("color12"))
        .or_else(|| m.get("foreground"))
        .copied()
        .unwrap_or(rgb(80, 200, 255));
    let boost = m.get("color2").copied().unwrap_or(rgb(70, 200, 90));
    let cut = m.get("color1").copied().unwrap_or(rgb(225, 80, 80));
    let highlight = m
        .get("color3")
        .or_else(|| m.get("color5"))
        .copied()
        .unwrap_or(Color32::YELLOW);
    Some(Palette {
        accent,
        boost,
        cut,
        neutral: m.get("color8").copied().unwrap_or(rgb(150, 150, 160)),
        graph_bg: bg,
        grid: lighten(bg, 1.6),
        highlight,
    })
}

/// Heuristic: a matugen background brighter than mid-grey ⇒ light theme.
fn matugen_is_light() -> bool {
    matugen_palette()
        .map(|p| {
            let c = p.graph_bg;
            (c.r() as u32 + c.g() as u32 + c.b() as u32) / 3 > 140
        })
        .unwrap_or(false)
}
