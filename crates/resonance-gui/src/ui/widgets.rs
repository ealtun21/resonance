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

/// Centre `add`'s content horizontally by its own measured width, so the
/// content keeps its natural size and only the side padding grows/shrinks as
/// the column resizes. Pads from *last frame's* measured width (kept in egui
/// memory) to avoid a layout feedback loop — so nothing inside `add` may size
/// itself to `ui.available_width()`, or the width would never settle.
pub(crate) fn centered<R>(
    ui: &mut egui::Ui,
    id_src: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let id = egui::Id::new(("centered", id_src));
    let avail = ui.available_width();
    let prev = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
    let pad = ((avail - prev) * 0.5).max(0.0);
    let outer = ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.vertical(add)
    });
    let inner = outer.inner;
    ui.ctx()
        .data_mut(|d| d.insert_temp(id, inner.response.rect.width()));
    inner.inner
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

/// Paint a centre-out gain bar in a fixed-size cell: a centre tick with the bar
/// growing right for boosts and left for cuts, scaled to ±`DB_RANGE`.
pub(crate) fn gain_bar(ui: &mut egui::Ui, db: f64, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(96.0, 14.0), egui::Sense::hover());
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
