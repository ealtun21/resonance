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
    /// Native accent + the OS's current light/dark preference (auto-follows).
    /// The default, so the app follows the desktop's light/dark setting.
    System,
    /// Native accent forced dark, for users who want to pick it manually.
    NativeDark,
    /// Native accent forced light, for users who want to pick it manually.
    NativeLight,
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
    pub const ALL: [Theme; 9] = [
        Theme::System,
        Theme::NativeDark,
        Theme::NativeLight,
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
            Theme::System => "Native (auto)",
            Theme::NativeDark => "Native Dark",
            Theme::NativeLight => "Native Light",
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
            Theme::System => native_palette(),
            Theme::NativeDark => native_palette_for(true),
            Theme::NativeLight => native_palette_for(false),
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
    pub(crate) fn is_light(self) -> bool {
        match self {
            Theme::Light | Theme::NativeLight => true,
            Theme::System => !system_is_dark(),
            Theme::Matugen => matugen_is_light(),
            _ => false,
        }
    }

    /// `(caption_fill, caption_text)` for theming the native Windows title bar so
    /// it blends into the app: the caption uses `panel_fill` — the exact colour
    /// of the toolbar that sits directly beneath it — so there's no seam between
    /// the title bar and the toolbar, with the toolbar's text colour for the
    /// title.
    #[cfg(target_os = "windows")]
    pub(crate) fn native_caption_colors(self) -> (egui::Color32, egui::Color32) {
        let v = self.visuals();
        (v.panel_fill, v.text_color())
    }

    /// Build the full `egui::Visuals` for this theme.
    pub fn visuals(self) -> egui::Visuals {
        let p = self.palette();
        let mut v = if self.is_light() {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        // Surface tiers give the UI depth instead of one flat slab: the window
        // base sits darkest, panels a step up, and section cards (faint_bg) a
        // step above that. The plot keeps the deepest `graph_bg`.
        let (base, panel, card) = if self.is_light() {
            (
                lighten(p.graph_bg, 1.0),
                darken(p.graph_bg, 1.04),
                rgb(255, 255, 255),
            )
        } else {
            (
                darken(p.graph_bg, 1.5),
                darken(p.graph_bg, 1.12),
                lighten(p.graph_bg, 1.22),
            )
        };
        v.window_fill = base;
        v.panel_fill = panel;
        v.extreme_bg_color = p.graph_bg;
        v.faint_bg_color = card;
        v.window_stroke = egui::Stroke::new(1.0, blend(panel, p.grid, 0.7));
        v.selection.bg_fill = p.accent.gamma_multiply(0.5);
        v.selection.stroke = egui::Stroke::new(1.0, p.accent);
        v.hyperlink_color = p.accent;

        // Platform-appropriate corner radius: KDE Breeze is subtle (~3px); macOS
        // and Windows 11 are a touch rounder. Keeps controls feeling native.
        let r = native_radius();
        v.window_corner_radius = egui::CornerRadius::same(r);
        v.menu_corner_radius = egui::CornerRadius::same(r);
        let wr = egui::CornerRadius::same(r.saturating_sub(1).max(2));
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = wr;
        }

        // Faint borders so panels/cards/inputs read as distinct surfaces.
        let hairline = blend(card, p.grid, 0.55);
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, hairline);
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, hairline);
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.2, p.accent);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.2, p.accent);

        // Buttons: accent-tinted off the card surface with a clear hover/active
        // progression so controls read as interactive, not flat grey.
        let btn = blend(card, p.accent, 0.20);
        v.widgets.inactive.weak_bg_fill = btn;
        v.widgets.inactive.bg_fill = btn;
        v.widgets.hovered.weak_bg_fill = blend(card, p.accent, 0.40);
        v.widgets.hovered.bg_fill = blend(card, p.accent, 0.40);
        v.widgets.active.weak_bg_fill = p.accent.gamma_multiply(0.85);
        v.widgets.active.bg_fill = p.accent.gamma_multiply(0.85);
        v
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn darken(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (f32::from(c.r()) / f) as u8,
        (f32::from(c.g()) / f) as u8,
        (f32::from(c.b()) / f) as u8,
    )
}

fn lighten(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (f32::from(c.r()) * f).min(255.0) as u8,
        (f32::from(c.g()) * f).min(255.0) as u8,
        (f32::from(c.b()) * f).min(255.0) as u8,
    )
}

/// Linear interpolation `a → b` by `t` (0..1), per channel.
fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

// ── Platform-native accent + light/dark detection ───────────────────────────
//
// The "Native" theme adopts the host desktop's accent colour and light/dark
// preference so the app reads as a native KDE / Windows / macOS app rather than
// a generic toolkit window. These are queried occasionally (on theme load), not
// per frame, so a file read / short subprocess is fine.

/// Build the Native palette from the system accent + light/dark, flat and
/// accent-led the way the host toolkits are.
fn native_palette() -> Palette {
    native_palette_for(system_is_dark())
}

/// The Native palette built around the system accent, forced to a `dark` or
/// light base. `Native (auto)` passes the detected OS mode; the manual
/// `Native Dark` / `Native Light` variants force their own.
fn native_palette_for(dark: bool) -> Palette {
    // Breeze blue is the sensible fallback when no accent can be read.
    let accent = system_accent().unwrap_or_else(|| rgb(61, 174, 233));
    if dark {
        Palette {
            accent,
            boost: rgb(80, 200, 120),
            cut: rgb(230, 90, 95),
            neutral: rgb(150, 152, 160),
            graph_bg: rgb(24, 25, 28),
            grid: rgb(60, 62, 68),
            highlight: lighten(accent, 1.45),
        }
    } else {
        Palette {
            accent,
            boost: rgb(40, 160, 80),
            cut: rgb(200, 60, 60),
            neutral: rgb(110, 112, 120),
            graph_bg: rgb(248, 249, 251),
            grid: rgb(206, 209, 215),
            highlight: darken(accent, 1.15),
        }
    }
}

/// The host desktop's accent colour, if discoverable.
// Returns Option for the cross-platform contract: the Linux/Windows accent
// lookups can fail (None). The macOS branch always resolves to Some, which trips
// unnecessary_wraps only on the macOS build — the Option is still required.
#[allow(clippy::unnecessary_wraps)]
fn system_accent() -> Option<Color32> {
    #[cfg(target_os = "linux")]
    {
        kde_accent()
    }
    #[cfg(target_os = "windows")]
    {
        windows_accent()
    }
    #[cfg(target_os = "macos")]
    {
        Some(macos_accent())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Native control corner radius for the host platform (KDE Breeze is subtle;
/// macOS/Windows 11 are slightly rounder).
pub(crate) fn native_radius() -> u8 {
    // KDE Breeze is subtle; macOS / Windows 11 are a touch rounder.
    if cfg!(target_os = "linux") { 3 } else { 6 }
}

/// Whether the host desktop is in dark mode (defaults to dark when unknown).
fn system_is_dark() -> bool {
    #[cfg(target_os = "linux")]
    {
        kde_is_dark().unwrap_or(true)
    }
    #[cfg(target_os = "windows")]
    {
        windows_is_dark().unwrap_or(true)
    }
    #[cfg(target_os = "macos")]
    {
        macos_is_dark()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        true
    }
}

#[cfg(target_os = "linux")]
fn kde_globals() -> Option<String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    std::fs::read_to_string(base.join("kdeglobals")).ok()
}

/// Pull a `key=R,G,B` value from an INI-style `[section]` in kdeglobals.
#[cfg(target_os = "linux")]
fn kde_color(text: &str, section: &str, key: &str) -> Option<Color32> {
    let start = text.find(&format!("[{section}]"))?;
    let rest = &text[start..];
    let end = rest[1..].find('[').map_or(rest.len(), |i| i + 1);
    for line in rest[..end].lines() {
        if let Some(v) = line.trim().strip_prefix(&format!("{key}=")) {
            let c: Vec<u8> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            if c.len() >= 3 {
                return Some(rgb(c[0], c[1], c[2]));
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn kde_accent() -> Option<Color32> {
    let t = kde_globals()?;
    // Plasma 5.23+ stores a user accent here; otherwise fall back to the colour
    // scheme's selection background (the colour KDE highlights with).
    kde_color(&t, "General", "AccentColor")
        .or_else(|| kde_color(&t, "Colors:Selection", "BackgroundNormal"))
}

#[cfg(target_os = "linux")]
fn kde_is_dark() -> Option<bool> {
    let t = kde_globals()?;
    let bg = kde_color(&t, "Colors:Window", "BackgroundNormal")?;
    Some((u32::from(bg.r()) + u32::from(bg.g()) + u32::from(bg.b())) / 3 < 128)
}

/// Read a `HKCU` registry value via `reg query`, returning the raw token (the
/// last whitespace field of the matching line). `CREATE_NO_WINDOW` keeps the
/// GUI-subsystem process from flashing a console.
#[cfg(target_os = "windows")]
fn reg_query(path: &str, value: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("reg")
        .args(["query", path, "/v", value])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .find(|l| l.contains(value))
        .and_then(|l| l.split_whitespace().last())
        .map(str::to_owned)
}

#[cfg(target_os = "windows")]
fn windows_accent() -> Option<Color32> {
    let v = reg_query(r"HKCU\Software\Microsoft\Windows\DWM", "AccentColor")?;
    let n = u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()?;
    // DWM AccentColor is 0xAABBGGRR.
    Some(rgb(
        (n & 0xFF) as u8,
        ((n >> 8) & 0xFF) as u8,
        ((n >> 16) & 0xFF) as u8,
    ))
}

#[cfg(target_os = "windows")]
fn windows_is_dark() -> Option<bool> {
    let v = reg_query(
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )?;
    let n = u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()?;
    Some(n == 0)
}

#[cfg(target_os = "macos")]
fn defaults_global(key: &str) -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(target_os = "macos")]
fn macos_accent() -> Color32 {
    // AppleAccentColor: -1 graphite, 0 red, 1 orange, 2 yellow, 3 green, 4 blue,
    // 5 purple, 6 pink. The key is absent when the user keeps the default blue.
    let idx = defaults_global("AppleAccentColor")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(4);
    match idx {
        -1 => rgb(140, 140, 148),
        0 => rgb(255, 82, 89),
        1 => rgb(247, 143, 42),
        2 => rgb(245, 200, 50),
        3 => rgb(98, 189, 62),
        5 => rgb(150, 90, 225),
        6 => rgb(245, 100, 170),
        _ => rgb(10, 110, 235),
    }
}

#[cfg(target_os = "macos")]
fn macos_is_dark() -> bool {
    defaults_global("AppleInterfaceStyle").is_some_and(|s| s.eq_ignore_ascii_case("Dark"))
}

// ── matugen / pywal palette loading ─────────────────────────────────────────

/// Candidate colour files, newest-style first. pywal and most matugen templates
/// write a JSON document with `special.background/foreground` and `colors.colorN`.
///
/// The platform-aware `colors.json` lives next to other Resonance config
/// (Linux: `~/.config/resonance`; macOS: `~/Library/Application Support/resonance`).
/// pywal's `~/.cache/wal/colors.json` is a Linux-only convention but harmless
/// to probe on macOS — it simply won't exist.
fn matugen_files() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    v.push(resonance_ipc::paths::config_dir().join("colors.json"));
    if let Ok(home) = std::env::var("HOME") {
        let h = std::path::PathBuf::from(home);
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
    // First non-empty key from the list, else the fallback. pywal/matugen files
    // vary wildly: some fill color0-15, some only a handful (the rest empty
    // strings, which never land in the map), some use material tokens. Walk a
    // broad chain per slot so a sparse palette still produces a usable theme
    // instead of silently falling back to the hard-coded default colours.
    let pick = |keys: &[&str], fallback: Color32| -> Color32 {
        keys.iter()
            .find_map(|k| m.get(*k).copied())
            .unwrap_or(fallback)
    };
    let bg = pick(&["background", "color0", "surface"], rgb(20, 22, 26));
    let fg = pick(
        &[
            "foreground",
            "color15",
            "color7",
            "onSurface",
            "onBackground",
        ],
        rgb(220, 220, 230),
    );
    let accent = pick(
        &[
            "color4", "color12", "color6", "color14", "primary", "color10", "color13", "color5",
        ],
        fg,
    );
    // Bright slots only — a dark "highlight" (e.g. color13 on some palettes)
    // makes the selected EQ node look greyed-out instead of standing out.
    let highlight = pick(
        &["color3", "color11", "color5", "color10", "secondary"],
        accent,
    );
    Some(Palette {
        accent,
        boost: pick(&["color2", "color10"], rgb(70, 200, 90)),
        cut: pick(&["color1", "color9"], rgb(225, 80, 80)),
        neutral: pick(&["color8", "color7"], blend(bg, fg, 0.55)),
        graph_bg: bg,
        // Blend bg toward fg so grid lines stay visible on any matugen palette
        // (a plain `lighten` of a dark bg stays dark and disappears).
        grid: blend(bg, fg, 0.45),
        highlight,
    })
}

/// Modification time of the active matugen/pywal colour file, if any exists.
/// Lets the app live-reload the Matugen theme when the file is rewritten.
pub fn matugen_source_mtime() -> Option<std::time::SystemTime> {
    let path = matugen_files().into_iter().find(|p| p.is_file())?;
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Heuristic: a matugen background brighter than mid-grey ⇒ light theme.
fn matugen_is_light() -> bool {
    matugen_palette().is_some_and(|p| {
        let c = p.graph_bg;
        (u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b())) / 3 > 140
    })
}
