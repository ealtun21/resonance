//! A small bespoke widget kit + design tokens for a consistent, pro-grade UI.
//!
//! Everything here is painter-drawn rather than egui's default widgets, so
//! spacing, type, alignment and styling stay uniform across every panel — one
//! source of truth for the look. Panels compose these primitives; they never
//! reach for `egui::Slider`/`Button`/`Checkbox` directly.
//!
//! Built out panel-by-panel, so some tokens/widgets land before every caller
//! exists; the module-level allow keeps that from tripping `-D warnings`.
#![allow(dead_code)]

use crate::ui::icons::{self, Icon};
use crate::ui::widgets::lerp_color;
use eframe::egui::{self, Color32};

// ── Design tokens ────────────────────────────────────────────────────────────

/// 4px spacing scale — every gap is one of these, nothing ad-hoc.
pub(crate) const SP_XS: f32 = 4.0;
pub(crate) const SP_S: f32 = 8.0;
pub(crate) const SP_M: f32 = 12.0;
pub(crate) const SP_L: f32 = 18.0;

/// Standard interactive row height (one control + its label).
pub(crate) const ROW_H: f32 = 30.0;
/// Height for medium controls that share a list row — buttons, text fields and
/// the right-column dropdowns/icon buttons — so they line up flush. The dense
/// bands table uses smaller controls and passes its own heights.
pub(crate) const CTRL_H: f32 = 26.0;
/// Type scale.
pub(crate) const T_CAPTION: f32 = 11.0;
pub(crate) const T_BODY: f32 = 13.0;
pub(crate) const T_VALUE: f32 = 12.5;

/// Resolved colour roles for the current theme (light or dark), derived once per
/// call from the active visuals + accent so widgets share exactly one palette.
pub(crate) struct Tokens {
    pub accent: Color32,
    pub text: Color32,
    pub dim: Color32,
    /// Inset control background (slider track, value chips, input wells).
    pub well: Color32,
    /// Hairline rule / borders.
    pub line: Color32,
}

pub(crate) fn tokens(ui: &egui::Ui) -> Tokens {
    let v = ui.visuals();
    let bg = v.panel_fill;
    let text = v.text_color();
    Tokens {
        accent: v.hyperlink_color,
        text,
        dim: v.weak_text_color(),
        well: lerp_color(bg, text, if v.dark_mode { 0.16 } else { 0.10 }),
        line: lerp_color(bg, text, if v.dark_mode { 0.22 } else { 0.16 }),
    }
}

/// Width a string occupies at a proportional `size`, measured against the live
/// fonts — so width-driven collapse logic tracks the real text size on any
/// machine (font/zoom) instead of assuming fixed pixels.
pub(crate) fn text_width(ui: &egui::Ui, size: f32, s: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            s.to_owned(),
            egui::FontId::proportional(size),
            Color32::WHITE,
        )
        .rect
        .width()
}

// ── Primitives ────────────────────────────────────────────────────────────────

/// A section header: a small upper-case accent caption with a hairline rule
/// filling the width beneath it. Consistent everywhere a group of controls
/// starts.
pub(crate) fn header(ui: &mut egui::Ui, title: &str) {
    let t = tokens(ui);
    ui.add_space(SP_XS);
    ui.label(
        egui::RichText::new(title.to_uppercase())
            .size(T_CAPTION)
            .strong()
            .color(t.accent),
    );
    ui.add_space(SP_XS);
    let (r, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.line);
    ui.add_space(SP_S);
}

/// A label + control row with a fixed-width label column so every row in a panel
/// aligns. `add` draws the control(s) in the remaining width.
pub(crate) fn control_row(
    ui: &mut egui::Ui,
    label_w: f32,
    label: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    let t = tokens(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(ROW_H);
        ui.spacing_mut().item_spacing.x = SP_S;
        let (lr, _) = ui.allocate_exact_size(egui::vec2(label_w, ROW_H), egui::Sense::hover());
        ui.painter().text(
            egui::pos2(lr.left(), lr.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(T_BODY),
            t.text,
        );
        add(ui);
    });
}

/// A bespoke horizontal slider: a thin rounded track, an accent-filled portion,
/// and a round handle. Returns true while being changed. Width is explicit so
/// rows stay aligned.
pub(crate) fn slider(
    ui: &mut egui::Ui,
    width: f32,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
) -> bool {
    let t = tokens(ui);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    let cy = rect.center().y;
    let pad = 8.0;
    let x0 = rect.left() + pad;
    let x1 = rect.right() - pad;
    let tw = (x1 - x0).max(1.0);
    let bipolar = lo < 0.0 && hi > 0.0;
    let frac = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    let hx = x0 + frac * tw;

    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = ((p.x - x0) / tw).clamp(0.0, 1.0) as f64;
            let nv = lo + f * (hi - lo);
            if (nv - *value).abs() > f64::EPSILON {
                *value = nv;
                changed = true;
            }
        }
    }

    let p = ui.painter();
    let track = egui::Rect::from_min_max(egui::pos2(x0, cy - 2.5), egui::pos2(x1, cy + 2.5));
    p.rect_filled(track, 2.5, t.well);
    // Filled portion: from the zero point for bipolar sliders, else from the left.
    let zero_x = if bipolar {
        x0 + ((0.0 - lo) / (hi - lo)) as f32 * tw
    } else {
        x0
    };
    let (fa, fb) = if hx >= zero_x {
        (zero_x, hx)
    } else {
        (hx, zero_x)
    };
    p.rect_filled(
        egui::Rect::from_min_max(egui::pos2(fa, cy - 2.5), egui::pos2(fb, cy + 2.5)),
        2.5,
        t.accent,
    );
    let r = if resp.hovered() || resp.dragged() {
        7.0
    } else {
        6.0
    };
    p.circle_filled(egui::pos2(hx, cy), r, t.accent);
    p.circle_filled(egui::pos2(hx, cy), r - 3.0, Color32::WHITE);
    changed
}

/// A bespoke on/off pill toggle (animated). Returns true when toggled.
pub(crate) fn toggle(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let t = tokens(ui);
    let size = egui::vec2(36.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let mut changed = false;
    if resp.clicked() {
        *on = !*on;
        changed = true;
    }
    let how = ui.ctx().animate_bool(resp.id, *on);
    let track = lerp_color(t.well, t.accent, how);
    let p = ui.painter();
    let radius = rect.height() * 0.5;
    p.rect_filled(rect, radius, track);
    let kr = radius - 2.5;
    let kx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how);
    p.circle_filled(egui::pos2(kx, rect.center().y), kr, Color32::WHITE);
    changed
}

/// Width of the soft fade applied to the edge of overflowing boxed text.
const FADE_W: f32 = 22.0;

/// Draw `text` left-aligned and clipped within `rect`. If it fits, draw it
/// plainly. If it overflows, fade the right edge into `bg` (a soft cut instead of
/// a hard clip or "…"), and while the pointer hovers `rect`, marquee-scroll the
/// text to reveal the whole string — pausing at each end — like a polished native
/// app. Use for any fixed-width box whose contents can be longer than the box
/// (dropdown values, device names).
pub(crate) fn fade_text(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    text: &str,
    font: egui::FontId,
    color: Color32,
    bg: Color32,
) {
    let pad = 7.0;
    let cy = rect.center().y;
    let painter = ui.painter_at(rect);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let tw = galley.size().x;
    let gy = cy - galley.size().y / 2.0;
    let avail = (rect.width() - pad * 2.0).max(1.0);
    if tw <= avail {
        painter.galley(egui::pos2(rect.left() + pad, gy), galley, color);
        return;
    }
    // Overflows: scroll while hovered, otherwise pin to the start. Hover is
    // occlusion-aware (so it doesn't scroll under a dialog/popup) and the phase
    // runs off the global clock — no stored per-box state to leak when a box
    // (e.g. a device row) disappears.
    let over = tw - avail + 2.0;
    let hovered = ui.ctx().rect_contains_pointer(ui.layer_id(), rect);
    let off = if hovered {
        ui.ctx().request_repaint(); // keep the scroll animating while hovered
        marquee_offset(ui.input(|i| i.time) as f32, over)
    } else {
        0.0
    };
    painter.galley(egui::pos2(rect.left() + pad - off, gy), galley, color);
    // Fade each edge by how much text still overhangs past it, so the fade masks
    // the hard clip but melts away as the marquee reveals the tail (or the head
    // once scrolled). At full scroll the right fade vanishes → the last glyphs
    // show fully.
    fade_strip(
        &painter,
        rect,
        bg,
        true,
        ((over - off) / FADE_W).clamp(0.0, 1.0),
    );
    fade_strip(&painter, rect, bg, false, (off / FADE_W).clamp(0.0, 1.0));
}

/// Marquee scroll offset (px) at `elapsed` seconds for content overhanging by
/// `over` px: hold at the start, scroll to the end, hold, scroll back, repeat.
fn marquee_offset(elapsed: f32, over: f32) -> f32 {
    const SPEED: f32 = 38.0; // px/s
    const PAUSE: f32 = 0.9; // s held at each end
    let travel = (over / SPEED).max(0.05);
    let cycle = 2.0 * (PAUSE + travel);
    let p = elapsed.rem_euclid(cycle);
    let o = if p < PAUSE {
        0.0
    } else if p < PAUSE + travel {
        (p - PAUSE) * SPEED
    } else if p < 2.0 * PAUSE + travel {
        over
    } else {
        over - (p - (2.0 * PAUSE + travel)) * SPEED
    };
    o.clamp(0.0, over)
}

/// Paint a horizontal alpha gradient over one edge of `rect` (transparent →
/// opaque `bg`), masking the text's hard clip. `right`: fade in toward the right
/// edge; else toward the left. `strength` (0..1) scales the peak opacity so the
/// fade can melt away as the marquee reveals that edge. Stepped strips avoid
/// needing a gradient mesh.
fn fade_strip(painter: &egui::Painter, rect: egui::Rect, bg: Color32, right: bool, strength: f32) {
    if strength <= 0.01 {
        return;
    }
    const N: usize = 14;
    let w = FADE_W / N as f32;
    for k in 0..N {
        let frac = k as f32 / (N as f32 - 1.0);
        let alpha = (if right { frac } else { 1.0 - frac }) * strength;
        let col = Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), (alpha * 255.0) as u8);
        let x = if right {
            rect.right() - FADE_W + k as f32 * w
        } else {
            rect.left() + k as f32 * w
        };
        let strip = egui::Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2(x + w + 0.5, rect.bottom()),
        );
        painter.rect_filled(strip, 0.0, col);
    }
}

/// A right-aligned monospace value chip (e.g. "+100%", "1.4 dB") in a `well`.
pub(crate) fn value_chip(ui: &mut egui::Ui, width: f32, text: &str) {
    let t = tokens(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, t.well);
    ui.painter().text(
        egui::pos2(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        text,
        egui::FontId::monospace(T_VALUE),
        t.text,
    );
}

/// A bespoke editable number: a monospace chip you drag horizontally to change,
/// or double-click to type into. Text entry uses a transient `TextEdit` (the one
/// place a platform text widget is unavoidable); the resting state is custom.
/// Returns true when the value changes.
pub(crate) fn num_field(
    ui: &mut egui::Ui,
    width: f32,
    id: egui::Id,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    decimals: usize,
    speed: f64,
) -> bool {
    let edit_key = id.with("editing");
    let mut changed = false;
    let editing: Option<String> = ui.data(|d| d.get_temp(edit_key));
    if let Some(mut buf) = editing {
        let out = ui.add_sized(
            [width, 22.0],
            egui::TextEdit::singleline(&mut buf)
                .id(id)
                .margin(egui::Margin::symmetric(6, 2))
                .horizontal_align(egui::Align::Center),
        );
        out.request_focus();
        if out.lost_focus() {
            if let Ok(v) = buf.trim().parse::<f64>() {
                let nv = v.clamp(*range.start(), *range.end());
                if (nv - *value).abs() > f64::EPSILON {
                    *value = nv;
                    changed = true;
                }
            }
            ui.data_mut(|d| d.remove::<String>(edit_key));
        } else {
            ui.data_mut(|d| d.insert_temp(edit_key, buf));
        }
    } else {
        let t = tokens(ui);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::click_and_drag());
        let bg = if resp.hovered() {
            lerp_color(t.well, t.accent, 0.14)
        } else {
            t.well
        };
        ui.painter().rect_filled(rect, 4.0, bg);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.*}", decimals, *value),
            egui::FontId::monospace(T_VALUE),
            t.text,
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            let dx = resp.drag_delta().x as f64;
            if dx != 0.0 {
                let nv = (*value + dx * speed).clamp(*range.start(), *range.end());
                if (nv - *value).abs() > f64::EPSILON {
                    *value = nv;
                    changed = true;
                }
            }
        }
        if resp.double_clicked() {
            ui.data_mut(|d| d.insert_temp(edit_key, format!("{:.*}", decimals, *value)));
        }
    }
    changed
}

/// A single-line text input styled to match the kit: a flat `well` chip with no
/// resting border, an accent focus ring, and the same 22 px height as the
/// dropdown / value chip / number field so it lines up in a row. The one place
/// (besides `num_field`'s typing mode) a platform text widget is unavoidable —
/// scoped visuals make egui's `TextEdit` wear the kit's flat look. Returns its
/// `Response` so callers handle focus/Enter/rename themselves.
pub(crate) fn text_field(
    ui: &mut egui::Ui,
    width: f32,
    id: egui::Id,
    buf: &mut String,
    hint: &str,
    accent_text: bool,
) -> egui::Response {
    let t = tokens(ui);
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        // The frame fill comes from `extreme_bg_color`; point it at the kit well
        // so the field matches the dropdown/chip surfaces instead of the deepest
        // plot background. Border off at rest, accent ring on focus.
        v.extreme_bg_color = t.well;
        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, lerp_color(t.well, t.accent, 0.5));
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, t.accent);
        v.selection.stroke = egui::Stroke::new(1.0, t.accent);
        let mut edit = egui::TextEdit::singleline(buf)
            .id(id)
            .hint_text(hint)
            .margin(egui::Margin::symmetric(7, 3))
            .desired_width(width);
        if accent_text {
            edit = edit.text_color(t.accent);
        }
        ui.add_sized([width, CTRL_H], edit)
    })
    .inner
}

/// A bespoke select: a chip showing the current option + a caret, opening a
/// custom popup list. Returns `Some(index)` when an option is chosen.
pub(crate) fn dropdown(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    popup_id: egui::Id,
    current: &str,
    options: &[&str],
) -> Option<usize> {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let bg = if resp.hovered() || open {
        lerp_color(t.well, t.accent, 0.16)
    } else {
        t.well
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    // The current value can be longer than the chip (long device/profile names):
    // clip it to the chip with a soft fade + hover-scroll, leaving room for the
    // caret on the right.
    let text_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 16.0, rect.max.y));
    fade_text(
        ui,
        text_rect,
        current,
        egui::FontId::proportional(T_VALUE),
        t.text,
        bg,
    );
    // Custom caret (the icon font lacks ▾, so draw it).
    let cx = rect.right() - 9.0;
    let cy = rect.center().y;
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(cx - 3.0, cy - 1.5),
            egui::pos2(cx + 3.0, cy - 1.5),
            egui::pos2(cx, cy + 2.5),
        ],
        t.dim,
        egui::Stroke::NONE,
    ));
    let mut sel = None;
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(width.max(110.0));
            let tk = tokens(ui);
            for (i, opt) in options.iter().enumerate() {
                let (r, rr) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 22.0),
                    egui::Sense::click(),
                );
                if rr.hovered() {
                    ui.painter()
                        .rect_filled(r, 3.0, tk.accent.gamma_multiply(0.30));
                }
                ui.painter().text(
                    egui::pos2(r.left() + 7.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    *opt,
                    egui::FontId::proportional(T_BODY),
                    tk.text,
                );
                if rr.clicked() {
                    sel = Some(i);
                }
            }
        });
    sel
}

// ── Vector-icon controls ─────────────────────────────────────────────────────

/// Paint a square vector-icon button and return its raw response. Drawn as a real
/// button — a resting `well` chip that brightens toward the accent on hover — so
/// it reads as a control, not a floating glyph. Senses clicks even when disabled
/// so a tooltip still shows.
fn vicon_button_draw(ui: &mut egui::Ui, icon: Icon, size: f32, enabled: bool) -> egui::Response {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let (bg, fg) = if !enabled {
        (t.well, t.dim)
    } else if resp.hovered() {
        (lerp_color(t.well, t.accent, 0.30), egui::Color32::WHITE)
    } else {
        (t.well, t.text)
    };
    ui.painter().rect_filled(rect, 5.0, bg);
    // Icon drawn in a centred box a bit smaller than the hit target.
    let g = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(size * 0.64));
    icons::draw(ui.painter(), icon, g, fg);
    resp
}

/// A square vector-icon button with a hover tooltip naming the action — the house
/// style for compact, repeated actions (load / delete / refresh / …). Returns
/// true on click.
pub(crate) fn icon_btn(ui: &mut egui::Ui, icon: Icon, size: f32, tip: &str) -> bool {
    let resp = vicon_button_draw(ui, icon, size, true);
    let clicked = resp.clicked();
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    clicked
}

/// [`icon_btn`] that can be drawn disabled (dim, click ignored) while still
/// surfacing its tooltip — so the user learns *why* it's unavailable.
pub(crate) fn icon_btn_enabled(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    enabled: bool,
    tip: &str,
) -> bool {
    let resp = vicon_button_draw(ui, icon, size, enabled);
    let clicked = enabled && resp.clicked();
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    clicked
}

/// A button with a leading vector icon then a label — for primary actions that
/// keep their text (Auto-EQ, Customize, Add band, Save). `accent` fills it.
fn icon_text_draw(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    accent: bool,
    enabled: bool,
) -> egui::Response {
    let t = tokens(ui);
    let font = egui::FontId::proportional(T_BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), t.text);
    let ico = CTRL_H * 0.62;
    let pad = 11.0;
    let gap = 6.0;
    let w = pad + ico + gap + galley.size().x + pad;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CTRL_H), egui::Sense::click());
    let (bg, fg) = if !enabled {
        (t.well, t.dim)
    } else if accent {
        (
            if resp.hovered() {
                t.accent
            } else {
                t.accent.gamma_multiply(0.88)
            },
            egui::Color32::WHITE,
        )
    } else if resp.hovered() {
        (lerp_color(t.well, t.accent, 0.22), t.text)
    } else {
        (t.well, t.text)
    };
    ui.painter().rect_filled(rect, 5.0, bg);
    let ig = egui::Rect::from_center_size(
        egui::pos2(rect.left() + pad + ico * 0.5, rect.center().y),
        egui::Vec2::splat(ico),
    );
    icons::draw(ui.painter(), icon, ig, fg);
    ui.painter().text(
        egui::pos2(ig.right() + gap, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    resp
}

/// An icon+label button with a hover tooltip (shown even when disabled). Returns
/// true on click.
pub(crate) fn icon_text_btn(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    accent: bool,
    enabled: bool,
    tip: &str,
) -> bool {
    let resp = icon_text_draw(ui, icon, label, accent, enabled);
    let clicked = enabled && resp.clicked();
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    clicked
}

/// Measured width of an [`icon_text_btn`] — for width-driven collapse logic.
pub(crate) fn icon_text_width(ui: &egui::Ui, label: &str) -> f32 {
    let ico = CTRL_H * 0.62;
    22.0 + ico + 6.0 + text_width(ui, T_BODY, label)
}

/// A vector-icon button that opens a popup menu (the ☰ overflow style). `tip`
/// names it on hover; `close_on_click` false keeps the menu open for in-menu
/// toggles. Mirrors [`menu_button_ex`] but with a crisp icon instead of a glyph.
pub(crate) fn icon_menu_button(
    ui: &mut egui::Ui,
    icon: Icon,
    popup_id: egui::Id,
    close_on_click: bool,
    tip: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), egui::Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let bg = if resp.hovered() || open {
        lerp_color(t.well, t.accent, 0.20)
    } else {
        t.well
    };
    ui.painter().rect_filled(rect, 5.0, bg);
    let g = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(16.0));
    icons::draw(ui.painter(), icon, g, t.text);
    if !tip.is_empty() {
        resp.clone().on_hover_text(tip);
    }
    let close = if close_on_click {
        egui::PopupCloseBehavior::CloseOnClick
    } else {
        egui::PopupCloseBehavior::CloseOnClickOutside
    };
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(close)
        .show(|ui| add(ui));
}

/// Paint a text button sized to its label and return its raw response. `accent`
/// fills it (primary actions); otherwise it sits in a `well`; `enabled == false`
/// dims it. Always senses clicks (callers gate the action on `enabled`) so a
/// disabled button still surfaces its hover tooltip explaining *why*.
fn button_draw(ui: &mut egui::Ui, label: &str, accent: bool, enabled: bool) -> egui::Response {
    let t = tokens(ui);
    let font = egui::FontId::proportional(T_BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), t.text);
    let w = galley.size().x + 24.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CTRL_H), egui::Sense::click());
    let (bg, fg) = if !enabled {
        (t.well, t.dim)
    } else if accent {
        (
            if resp.hovered() {
                t.accent
            } else {
                t.accent.gamma_multiply(0.88)
            },
            egui::Color32::WHITE,
        )
    } else if resp.hovered() {
        (lerp_color(t.well, t.accent, 0.22), t.text)
    } else {
        (t.well, t.text)
    };
    ui.painter().rect_filled(rect, 5.0, bg);
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, label, font, fg);
    resp
}

/// A bespoke text button, sized to its label. `accent` fills it (e.g. primary
/// actions); otherwise it sits in a `well`. `enabled == false` dims it and
/// ignores clicks.
pub(crate) fn button(ui: &mut egui::Ui, label: &str, accent: bool, enabled: bool) -> bool {
    enabled && button_draw(ui, label, accent, enabled).clicked()
}

/// Like [`button`] but with a hover tooltip (shown even when disabled, so the
/// user learns *why* an action is unavailable). Returns true on click.
pub(crate) fn button_tip(
    ui: &mut egui::Ui,
    label: &str,
    accent: bool,
    enabled: bool,
    tip: &str,
) -> bool {
    let resp = button_draw(ui, label, accent, enabled);
    let clicked = enabled && resp.clicked();
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    clicked
}

/// A kit button that toggles a popup anchored *directly below it* (not at the
/// panel's left edge). Unlike [`menu_button`], the popup stays open while you
/// interact with its contents (dragging sliders, typing) — it closes only on a
/// click outside — so it suits an inline editor rather than a one-shot menu. The
/// button is accent-filled while the popup is open.
pub(crate) fn popup_button(
    ui: &mut egui::Ui,
    label: &str,
    tip: &str,
    popup_id: egui::Id,
    min_width: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let resp = button_draw(ui, label, open, true);
    if !tip.is_empty() {
        resp.clone().on_hover_text(tip);
    }
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(min_width);
            add(ui);
        });
}

/// Like [`popup_button`] but with a leading vector icon — the icon language for
/// an inline editor trigger (e.g. the target Customize popup).
pub(crate) fn icon_popup_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    tip: &str,
    popup_id: egui::Id,
    min_width: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let resp = icon_text_draw(ui, icon, label, open, true);
    if !tip.is_empty() {
        resp.clone().on_hover_text(tip);
    }
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(min_width);
            add(ui);
        });
}

/// A bespoke menu button: a kit chip that opens a popup of menu content.
/// `label_color` lets callers tint the label (e.g. the daemon status dot).
/// Closes on any click inside (one-shot actions).
pub(crate) fn menu_button(
    ui: &mut egui::Ui,
    label: &str,
    label_color: Color32,
    popup_id: egui::Id,
    add: impl FnOnce(&mut egui::Ui),
) {
    menu_button_ex(ui, label, label_color, popup_id, true, "", add)
}

/// [`menu_button`] with two extra knobs: `close_on_click` false keeps the menu
/// open after a click (so in-menu toggles can be flipped repeatedly; it then
/// closes only on an outside click), and a hover `tip` on the button itself.
pub(crate) fn menu_button_ex(
    ui: &mut egui::Ui,
    label: &str,
    label_color: Color32,
    popup_id: egui::Id,
    close_on_click: bool,
    tip: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    let t = tokens(ui);
    let font = egui::FontId::proportional(T_BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), label_color);
    let w = galley.size().x + 22.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let bg = if resp.hovered() || open {
        lerp_color(t.well, t.accent, 0.20)
    } else {
        t.well
    };
    ui.painter().rect_filled(rect, 5.0, bg);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        label_color,
    );
    if !tip.is_empty() {
        resp.clone().on_hover_text(tip);
    }
    let close = if close_on_click {
        egui::PopupCloseBehavior::CloseOnClick
    } else {
        egui::PopupCloseBehavior::CloseOnClickOutside
    };
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(close)
        .show(|ui| add(ui));
}

/// A row inside a kit popup menu: full-width, hover-highlit, with an accent bar +
/// accent text when `checked` (e.g. the active theme). Returns true on click.
pub(crate) fn menu_item(ui: &mut egui::Ui, label: &str, checked: bool) -> bool {
    let t = tokens(ui);
    let w = ui.available_width().max(186.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 27.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, t.accent.gamma_multiply(0.22));
    }
    if checked {
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + 3.0, rect.top() + 6.0),
                egui::pos2(rect.left() + 6.0, rect.bottom() - 6.0),
            ),
            1.5,
            t.accent,
        );
    }
    ui.painter().text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(T_BODY),
        if checked { t.accent } else { t.text },
    );
    resp.clicked()
}

/// A small caption label for grouping rows inside a kit popup menu.
pub(crate) fn menu_caption(ui: &mut egui::Ui, text: &str) {
    let t = tokens(ui);
    ui.add_space(SP_XS);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(T_CAPTION - 0.5)
            .color(t.dim),
    );
    ui.add_space(SP_XS / 2.0);
}
