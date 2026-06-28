//! Shared, stateless GUI widget helpers: font installation, text/layout
//! utilities, colour math, and the centre-out gain bar.

use crate::curve;
use crate::state::GAIN_LIMIT;
use crate::theme::Palette;
use eframe::egui;

/// Bundled icon font: a ~2 KB subset of DejaVu Sans containing only the eight
/// glyphs the UI draws (●▸↑✕✚→·…), which egui's built-in fonts lack. Embedded
/// so icons render identically everywhere with negligible binary cost.
/// DejaVu license; see `assets/DejaVuSans-LICENSE.txt`.
const SYMBOL_FONT: &[u8] = include_bytes!("../../assets/icons.ttf");

/// Register the bundled symbol font as a fallback so the geometric glyphs used
/// in the UI (●, ▸, ↑, ✕, ✚, →, …) render instead of tofu boxes — egui's
/// built-in fonts cover only a small symbol subset. Appended last so normal
/// text keeps the default typeface.
pub(crate) fn install_symbol_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-symbols".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(SYMBOL_FONT)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("noto-symbols".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// A section drawn as a rounded card: a faint raised surface with a hairline
/// border, a caps header over a thin rule, then the body. Each control group
/// reads as its own container (the design-mock look) instead of bleeding into one
/// flat panel. Fills the column width so cards span their pane.
pub(crate) fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    crate::ui::kit::card(ui, title, "", None, add);
}

/// A section card with a right-aligned hint in its head bar (mockup `.card>.head`
/// trailing text — e.g. "FxSound chain", "6 bands · 1 off").
pub(crate) fn section_hint(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    crate::ui::kit::card(ui, title, hint, None, add);
}

/// A flat collapsible section for the narrow accordion layout: a native
/// disclosure header, no surrounding box. `id` keys the persisted open/closed
/// state; `default_open` applies only the first time the id is seen.
pub(crate) fn accordion(
    ui: &mut egui::Ui,
    id: &str,
    title: &str,
    default_open: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    crate::ui::kit::accordion(ui, id, title, default_open, body);
}

/// A modal dialog window sized to fit the current viewport: centred, resizable,
/// capped at 90% of the window (floor 320×240) so it never overflows a small
/// window, and opening at a comfortable default width. Replaces the old
/// hardcoded `default_size`/`min_width` that didn't fit narrow windows.
pub(crate) fn dialog_window<'a>(ctx: &egui::Context, title: &'a str) -> egui::Window<'a> {
    let vp = ctx.content_rect().size();
    let max_w = (vp.x * 0.9).max(320.0);
    let max_h = (vp.y * 0.9).max(240.0);
    // Centre on first open via pivot+default_pos (NOT `.anchor`, which pins the
    // window so it can't be dragged); egui then remembers the user's position,
    // making the dialog movable.
    egui::Window::new(title)
        .collapsible(false)
        .resizable(true)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .max_width(max_w)
        .max_height(max_h)
        .default_width(max_w.min(620.0))
}

/// Truncate `s` to at most `max` chars, appending an ellipsis when cut. Keeps
/// the toolbar output combo from expanding past its slot on long device names.
pub(crate) fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        format!("{}…", s.chars().take(keep).collect::<String>())
    }
}

/// Wrap a column's content in uniform inner padding and a vertical scroll area,
/// so the three central columns breathe instead of hugging the panel edge.
pub(crate) fn padded_scroll(ui: &mut egui::Ui, salt: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(8, 10))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(salt)
                .auto_shrink([false, false])
                .show(ui, add);
        });
}

/// A colour that contrasts strongly with `bg`: near-white on dark backgrounds,
/// near-black on light ones. Used for UI guides that must read on any theme.
pub(crate) fn contrast_color(bg: egui::Color32) -> egui::Color32 {
    let lum = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if lum < 128.0 {
        egui::Color32::from_rgb(245, 245, 250)
    } else {
        egui::Color32::from_rgb(15, 15, 20)
    }
}

pub(crate) fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    egui::Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Colour for a gain value: neutral accent near 0, tinting toward boost (green)
/// or cut (red) as magnitude grows — the FR curve's colour coding.
pub(crate) fn gain_color(db: f64, pal: &Palette) -> egui::Color32 {
    let t = (db.abs() / GAIN_LIMIT).clamp(0.0, 1.0) as f32;
    if db.abs() < 0.3 {
        return pal.accent;
    }
    let target = if db > 0.0 { pal.boost } else { pal.cut };
    lerp_color(pal.accent, target, t)
}

/// Colour for a frequency, mapped across the audible band like visible light —
/// deep red in the sub-bass, through green in the mids, to blue/violet in the
/// treble — so a band's frequency reads at a glance. Log-scaled 20 Hz → 20 kHz.
pub(crate) fn freq_color(freq: f64) -> egui::Color32 {
    // (position 0..1, r, g, b) low→high; kept legible on the dark `well`.
    const STOPS: [(f32, u8, u8, u8); 7] = [
        (0.00, 226, 108, 108), // 20 Hz   red
        (0.17, 226, 150, 84),  // ~60 Hz  orange
        (0.34, 220, 200, 100), // ~250 Hz yellow
        (0.50, 124, 202, 134), // ~1 kHz  green
        (0.66, 96, 202, 202),  // ~4 kHz  cyan
        (0.83, 104, 162, 236), // ~10 kHz blue
        (1.00, 172, 134, 236), // 20 kHz  violet
    ];
    let lmin = 20f64.log10();
    let lmax = 20_000f64.log10();
    let t = (((freq.max(1.0).log10() - lmin) / (lmax - lmin)) as f32).clamp(0.0, 1.0);
    let mut i = 0;
    while i + 1 < STOPS.len() - 1 && t > STOPS[i + 1].0 {
        i += 1;
    }
    let (t0, r0, g0, b0) = STOPS[i];
    let (t1, r1, g1, b1) = STOPS[i + 1];
    let f = if (t1 - t0).abs() < 1e-6 {
        0.0
    } else {
        ((t - t0) / (t1 - t0)).clamp(0.0, 1.0)
    };
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f) as u8;
    egui::Color32::from_rgb(l(r0, r1), l(g0, g1), l(b0, b1))
}

/// Paint a centre-out gain bar in a `width`×14 cell: a centre tick with the bar
/// growing right for boosts and left for cuts, scaled to ±`DB_RANGE`.
pub(crate) fn gain_bar(ui: &mut egui::Ui, width: f32, db: f64, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width.max(40.0), 14.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let cx = rect.center().x;
    // Centre tick.
    painter.line_segment(
        [
            egui::pos2(cx, rect.top() + 1.0),
            egui::pos2(cx, rect.bottom() - 1.0),
        ],
        egui::Stroke::new(1.0, pal.grid),
    );
    // Scale to the FR graph's ±DB_RANGE so the bar length matches the curve's
    // vertical extent (and the TUI's bar), not the much larger ±GAIN_LIMIT edit
    // clamp — which made a typical ±6 dB edit read as a sliver.
    let t = (db / curve::DB_RANGE).clamp(-1.0, 1.0) as f32;
    let half = rect.width() * 0.5 - 2.0;
    let w = t.abs() * half;
    if w >= 1.0 {
        let bar = if db >= 0.0 {
            egui::Rect::from_min_max(
                egui::pos2(cx, rect.top() + 2.0),
                egui::pos2(cx + w, rect.bottom() - 2.0),
            )
        } else {
            egui::Rect::from_min_max(
                egui::pos2(cx - w, rect.top() + 2.0),
                egui::pos2(cx, rect.bottom() - 2.0),
            )
        };
        painter.rect_filled(bar, 1.0, gain_color(db, pal));
    }
}
