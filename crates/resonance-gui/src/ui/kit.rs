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
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
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
    let (fa, fb) = if hx >= zero_x { (zero_x, hx) } else { (hx, zero_x) };
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

/// A bespoke select: a chip showing the current option + a caret, opening a
/// custom popup list. Returns `Some(index)` when an option is chosen.
pub(crate) fn dropdown(
    ui: &mut egui::Ui,
    width: f32,
    popup_id: egui::Id,
    current: &str,
    options: &[&str],
) -> Option<usize> {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let bg = if resp.hovered() || open {
        lerp_color(t.well, t.accent, 0.16)
    } else {
        t.well
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().text(
        egui::pos2(rect.left() + 7.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        current,
        egui::FontId::proportional(T_VALUE),
        t.text,
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

/// A small square icon button (✕, ✚, …). Returns true on click.
pub(crate) fn icon_button(ui: &mut egui::Ui, glyph: &str) -> bool {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, lerp_color(t.well, t.accent, 0.30));
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(13.0),
        if resp.hovered() { t.text } else { t.dim },
    );
    resp.clicked()
}

/// A bespoke text button, sized to its label. `accent` fills it (e.g. primary
/// actions); otherwise it sits in a `well`.
pub(crate) fn button(ui: &mut egui::Ui, label: &str, accent: bool) -> bool {
    let t = tokens(ui);
    let font = egui::FontId::proportional(T_BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), t.text);
    let w = galley.size().x + 24.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
    let (bg, fg) = if accent {
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
    resp.clicked()
}
