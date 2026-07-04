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

/// Corner radii (mockup `--r-card` / `--r-ctl`): cards are softly rounded,
/// inline controls a touch tighter, so the hierarchy reads at a glance.
pub(crate) const R_CARD: f32 = 8.0;
pub(crate) const R_CTRL: f32 = 5.0;
/// Card interior padding (mockup `.pad` / `.head`: 10px vertical, 12px
/// horizontal) and head-bar height (caption row over a full-width rule).
pub(crate) const CARD_PAD_X: f32 = 12.0;
pub(crate) const CARD_PAD_Y: f32 = 10.0;
pub(crate) const CARD_HEAD_H: f32 = 36.0;
/// Letter-spacing for the small upper-case section captions (mockup `.cap`).
const CAP_TRACKING: f32 = 1.1;

/// Resolved colour roles for the current theme (light or dark), derived once per
/// call from the active visuals + accent so widgets share exactly one palette.
pub(crate) struct Tokens {
    pub accent: Color32,
    pub text: Color32,
    pub dim: Color32,
    /// Even dimmer than `dim` — section captions, hints, inactive labels
    /// (mockup `--faint`). Recedes without vanishing.
    pub faint: Color32,
    /// Inset control background (slider track, value chips, input wells).
    pub well: Color32,
    /// Hairline rule / borders.
    pub line: Color32,
}

pub(crate) fn tokens(ui: &egui::Ui) -> Tokens {
    let v = ui.visuals();
    let bg = v.panel_fill;
    let text = v.text_color();
    let dim = v.weak_text_color();
    Tokens {
        accent: v.hyperlink_color,
        text,
        dim,
        faint: lerp_color(bg, dim, if v.dark_mode { 0.62 } else { 0.70 }),
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

/// Paint a small upper-case caption (mockup `.cap`) left-anchored at `pos`
/// (vertically centred), with the design's letter-spacing. Returns its width so
/// callers can lay out trailing content. Used by the card head + table captions.
pub(crate) fn caption(painter: &egui::Painter, pos: egui::Pos2, text: &str, color: Color32) -> f32 {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(T_CAPTION),
            color,
            extra_letter_spacing: CAP_TRACKING,
            ..Default::default()
        },
    );
    let galley = painter.layout_job(job);
    let h = galley.size().y;
    let w = galley.size().x;
    painter.galley(egui::pos2(pos.x, pos.y - h / 2.0), galley, color);
    w
}

/// A bespoke card (mockup `.card`): a rounded surface with a hairline border, a
/// **head bar** (upper-case caption + an optional right-aligned hint, divided
/// from the body by a full-width rule), then the padded body. This is the one
/// container primitive — every control group is a card, so spacing, the head
/// rule and the body inset stay identical everywhere (replacing the ad-hoc
/// `egui::Frame` look whose inset rule floated instead of dividing head/body).
///
/// `fill_override` lets the hero graph use the deep plot surface while ordinary
/// cards take the raised card tier; `None` uses the theme's card surface.
pub(crate) fn card(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    fill_override: Option<Color32>,
    body: impl FnOnce(&mut egui::Ui),
) {
    card_impl(ui, title, hint, fill_override, CARD_PAD_X, true, body);
}

/// A [`card`] whose body has **no horizontal padding** — for a full-bleed table
/// (mockup `.bandscard`): the rows' tint, rules and selection bar run edge-to-edge
/// while the table itself insets its cells. The head keeps its padding. Not
/// collapsible (it's the central hero content).
pub(crate) fn card_flush(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    body: impl FnOnce(&mut egui::Ui),
) {
    card_impl(ui, title, hint, None, 0.0, false, body);
}

fn card_impl(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    fill_override: Option<Color32>,
    body_pad_x: f32,
    collapsible: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    let t = tokens(ui);
    let fill = fill_override.unwrap_or_else(|| ui.visuals().faint_bg_color);
    // Per-title collapsed state. Clicking the head toggles it; collapsed cards
    // shrink to just the rounded title bar (handy now that several stack up).
    let collapse_id = egui::Id::new(("card_collapsed", title));
    let mut collapsed =
        collapsible && ui.data(|d| d.get_temp::<bool>(collapse_id).unwrap_or(false));
    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, t.line))
        .corner_radius(egui::CornerRadius::same(R_CARD as u8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // Head bar: the content area spans the full card width (Frame adds no
            // margin), so the divider can run edge-to-edge under the caption.
            let full_w = ui.available_width();
            let sense = if collapsible {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let (head, head_resp) = ui.allocate_exact_size(egui::vec2(full_w, CARD_HEAD_H), sense);
            if head_resp.clicked() {
                collapsed = !collapsed;
                ui.data_mut(|d| d.insert_temp(collapse_id, collapsed));
            }
            if collapsible && head_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let p = ui.painter();
            // Disclosure triangle (collapsible cards only), painter-drawn so it
            // never depends on a font glyph; the title sits after it. Points down
            // when expanded, right when collapsed.
            let title_x = if collapsible {
                let col = if head_resp.hovered() { t.text } else { t.dim };
                let cx = head.left() + CARD_PAD_X + 4.0;
                let cy = head.center().y;
                let tri = if collapsed {
                    vec![
                        egui::pos2(cx - 3.0, cy - 4.0),
                        egui::pos2(cx - 3.0, cy + 4.0),
                        egui::pos2(cx + 4.0, cy),
                    ]
                } else {
                    vec![
                        egui::pos2(cx - 4.0, cy - 3.0),
                        egui::pos2(cx + 4.0, cy - 3.0),
                        egui::pos2(cx, cy + 4.0),
                    ]
                };
                p.add(egui::Shape::convex_polygon(tri, col, egui::Stroke::NONE));
                head.left() + CARD_PAD_X + 16.0
            } else {
                head.left() + CARD_PAD_X
            };
            caption(p, egui::pos2(title_x, head.center().y), title, t.dim);
            if !hint.is_empty() {
                p.text(
                    egui::pos2(head.right() - CARD_PAD_X, head.center().y),
                    egui::Align2::RIGHT_CENTER,
                    hint,
                    egui::FontId::proportional(T_CAPTION),
                    t.faint,
                );
            }
            // Divider + body only when expanded; collapsed = clean rounded title bar.
            if !collapsed {
                p.hline(
                    head.x_range(),
                    head.bottom() - 0.5,
                    egui::Stroke::new(1.0, t.line),
                );
                egui::Frame::default()
                    .inner_margin(egui::Margin {
                        left: body_pad_x as i8,
                        right: body_pad_x as i8,
                        top: CARD_PAD_Y as i8,
                        bottom: CARD_PAD_Y as i8,
                    })
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        body(ui);
                    });
            }
        });
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
    slider_h(ui, width, ROW_H, value, range)
}

/// [`slider`] with an explicit row height — a shorter row gives the demoted look
/// (e.g. the effects sliders) while keeping the same track/handle styling.
pub(crate) fn slider_h(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
) -> bool {
    let t = tokens(ui);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
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
            let f = f64::from(((p.x - x0) / tw).clamp(0.0, 1.0));
            let mut nv = lo + f * (hi - lo);
            // Bipolar sliders (preamp, bass, …) snap to exactly 0 within a small
            // dead zone around the centre, so the neutral value is reachable
            // instead of sticking just off zero (e.g. 0.1 dB).
            if bipolar {
                let zero_x = x0 + ((0.0 - lo) / (hi - lo)) as f32 * tw;
                if (p.x - zero_x).abs() <= 3.0 {
                    nv = 0.0;
                }
            }
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

/// A bespoke checkbox (mockup `.ck`): a small rounded box that fills with the
/// accent and shows a check when on, plus a trailing label. Returns true when
/// toggled. The house replacement for `egui::Checkbox`/`selectable_label` in
/// menus, popups and dialogs.
pub(crate) fn checkbox(ui: &mut egui::Ui, on: &mut bool, label: &str) -> bool {
    const BOX: f32 = 16.0;
    let t = tokens(ui);
    let font = egui::FontId::proportional(T_BODY);
    let lab_w = if label.is_empty() {
        0.0
    } else {
        text_width(ui, T_BODY, label) + SP_S
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(BOX + lab_w, ROW_H), egui::Sense::click());
    let mut changed = false;
    if resp.clicked() {
        *on = !*on;
        changed = true;
    }
    let cy = rect.center().y;
    let box_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + BOX * 0.5, cy),
        egui::Vec2::splat(BOX),
    );
    let p = ui.painter();
    let how = ui.ctx().animate_bool(resp.id, *on);
    let fill = lerp_color(t.well, t.accent, how);
    let border = lerp_color(t.line, t.accent, how);
    p.rect_filled(box_rect, 3.0, fill);
    p.rect_stroke(
        box_rect,
        3.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    if how > 0.05 {
        // A check mark, drawn as two strokes so it reads crisp at this size.
        let c = box_rect.center();
        let col = egui::Color32::WHITE.gamma_multiply(how);
        let s = BOX * 0.5;
        p.add(egui::Shape::line(
            vec![
                egui::pos2(c.x - s * 0.42, c.y + s * 0.02),
                egui::pos2(c.x - s * 0.08, c.y + s * 0.34),
                egui::pos2(c.x + s * 0.46, c.y - s * 0.34),
            ],
            egui::Stroke::new(1.8, col),
        ));
    }
    if !label.is_empty() {
        p.text(
            egui::pos2(box_rect.right() + SP_S, cy),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            t.text,
        );
    }
    changed
}

// ── Pills (reference bar) ─────────────────────────────────────────────────────

/// Fully-rounded pill height (mockup `.pill`).
pub(crate) const PILL_H: f32 = 28.0;

/// Paint a rounded pill and return its raw response. May carry a leading check
/// box and/or icon before the label. States: `active` (accent-tint fill + accent
/// text — the on/selected look), `ghost` (transparent until hovered), else a
/// `well` chip. `open` forces the active look (popup pills). Disabled dims it.
#[allow(clippy::too_many_arguments)]
fn pill_draw(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    check: Option<bool>,
    label: &str,
    active: bool,
    ghost: bool,
    enabled: bool,
    open: bool,
) -> egui::Response {
    let t = tokens(ui);
    let font = egui::FontId::proportional(T_VALUE);
    let pad = 11.0;
    let gap = 6.0;
    let has_label = !label.is_empty();
    // A leading element only reserves a trailing gap when something follows it,
    // so an icon-only pill stays centred instead of sitting gap-width left.
    let ck_w = if check.is_some() { 14.0 + gap } else { 0.0 };
    let ic_w = if icon.is_some() {
        14.0 + if has_label { gap } else { 0.0 }
    } else {
        0.0
    };
    let lab_w = if has_label {
        text_width(ui, T_VALUE, label)
    } else {
        0.0
    };
    let w = pad + ck_w + ic_w + lab_w + pad;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, PILL_H), egui::Sense::click());
    let hot = active || open;
    let hover = resp.hovered() && enabled;
    let (bg, border, fg) = if !enabled {
        (
            if ghost { Color32::TRANSPARENT } else { t.well },
            t.line,
            t.faint,
        )
    } else if hot {
        (
            lerp_color(t.well, t.accent, if hover { 0.30 } else { 0.20 }),
            lerp_color(t.line, t.accent, 0.65),
            t.accent,
        )
    } else if ghost {
        (
            if hover {
                t.accent.gamma_multiply(0.12)
            } else {
                Color32::TRANSPARENT
            },
            t.line,
            if hover { t.text } else { t.dim },
        )
    } else {
        (
            if hover {
                lerp_color(t.well, t.accent, 0.14)
            } else {
                t.well
            },
            t.line,
            t.text,
        )
    };
    let radius = PILL_H / 2.0;
    let p = ui.painter();
    p.rect_filled(rect, radius, bg);
    p.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let cy = rect.center().y;
    let mut x = rect.left() + pad;
    if let Some(on) = check {
        let bx = egui::Rect::from_center_size(egui::pos2(x + 7.0, cy), egui::Vec2::splat(14.0));
        p.rect_filled(bx, 3.0, if on { t.accent } else { t.well });
        p.rect_stroke(
            bx,
            3.0,
            egui::Stroke::new(1.0, if on { t.accent } else { t.line }),
            egui::StrokeKind::Inside,
        );
        if on {
            let c = bx.center();
            p.add(egui::Shape::line(
                vec![
                    egui::pos2(c.x - 3.0, c.y),
                    egui::pos2(c.x - 1.0, c.y + 2.6),
                    egui::pos2(c.x + 3.2, c.y - 2.6),
                ],
                egui::Stroke::new(1.6, Color32::WHITE),
            ));
        }
        x += 14.0 + gap;
    }
    if let Some(ic) = icon {
        let g = egui::Rect::from_center_size(egui::pos2(x + 7.0, cy), egui::Vec2::splat(14.0));
        icons::draw(p, ic, g, fg);
        x += 14.0 + gap;
    }
    if !label.is_empty() {
        p.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            fg,
        );
    }
    resp
}

/// A label-only pill that toggles a boolean *checkbox* inside it (mockup
/// Raw/Bounds/Normalize/Reference). `on` drives both the check and the active
/// tint. Returns true on click. `tip` shows on hover.
pub(crate) fn pill_check(ui: &mut egui::Ui, label: &str, on: bool, tip: &str) -> bool {
    let resp = pill_draw(ui, None, Some(on), label, on, false, true, false);
    let clicked = resp.clicked();
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    clicked
}

/// A pill with an optional leading icon + label. `active` gives the accent look
/// (e.g. Auto-EQ). `ghost` is transparent-until-hover (icon-only secondary
/// actions). Returns true on click; disabled still surfaces `tip`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pill_icon(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    active: bool,
    ghost: bool,
    enabled: bool,
    tip: &str,
) -> bool {
    let resp = pill_draw(ui, icon, None, label, active, ghost, enabled, false);
    let clicked = enabled && resp.clicked();
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    clicked
}

/// A pill that opens a popup anchored below it (mockup Customize). Active-styled
/// while open; the popup stays open until an outside click.
pub(crate) fn pill_popup(
    ui: &mut egui::Ui,
    icon: Option<Icon>,
    label: &str,
    tip: &str,
    popup_id: egui::Id,
    min_width: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let resp = pill_draw(ui, icon, None, label, false, true, true, open);
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
    const N: usize = 14;
    if strength <= 0.01 {
        return;
    }
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
    num_field_impl(ui, width, id, value, range, decimals, speed, None)
}

/// As [`num_field`] but the resting value is tinted `color` (e.g. gain
/// green/red); the typing state stays default for legibility.
#[allow(clippy::too_many_arguments)]
pub(crate) fn num_field_colored(
    ui: &mut egui::Ui,
    width: f32,
    id: egui::Id,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    decimals: usize,
    speed: f64,
    color: Color32,
) -> bool {
    num_field_impl(ui, width, id, value, range, decimals, speed, Some(color))
}

#[allow(clippy::too_many_arguments)]
fn num_field_impl(
    ui: &mut egui::Ui,
    width: f32,
    id: egui::Id,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    decimals: usize,
    speed: f64,
    color: Option<Color32>,
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
            color.unwrap_or(t.text),
        );
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            let dx = f64::from(resp.drag_delta().x);
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

/// A two-line variant of [`dropdown`] (mockup `.device` chip): a bold primary
/// line over a dim mono secondary line, with a caret + popup list. For the output
/// device picker, where the friendly name and the node id both matter.
pub(crate) fn dropdown_2line(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    popup_id: egui::Id,
    line1: &str,
    line2: &str,
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
    ui.painter().rect_filled(rect, R_CTRL, bg);
    let right = rect.right() - 16.0;
    let cy = rect.center().y;
    let l1 = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 3.0, rect.top() + 2.0),
        egui::pos2(right, cy + 1.0),
    );
    let l2 = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 3.0, cy - 1.0),
        egui::pos2(right, rect.bottom() - 2.0),
    );
    fade_text(
        ui,
        l1,
        line1,
        egui::FontId::proportional(T_VALUE),
        t.text,
        bg,
    );
    fade_text(
        ui,
        l2,
        line2,
        egui::FontId::monospace(T_CAPTION - 0.5),
        t.faint,
        bg,
    );
    let cx = rect.right() - 9.0;
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
            ui.set_min_width(width.max(150.0));
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

/// A compact bordered tag-chip that opens a single-select popup — like
/// [`dropdown`] but the value is a short, centred, *coloured* label in a bordered
/// chip (e.g. the band-type PK/LS/HS badge). Returns `Some(index)` on choose.
pub(crate) fn tag_dropdown(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    popup_id: egui::Id,
    current: &str,
    text_color: Color32,
    options: &[&str],
) -> Option<usize> {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let border = if resp.hovered() || open {
        t.accent
    } else {
        lerp_color(t.well, text_color, 0.45)
    };
    ui.painter().rect_filled(rect, 4.0, t.well);
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        current,
        egui::FontId::monospace(T_VALUE),
        text_color,
    );
    let mut sel = None;
    egui::Popup::menu(&resp)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(width.max(120.0));
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

/// A bordered tag-chip with a centred coloured label + a small caret, returning
/// its click `Response` so the caller can anchor its own popup (used for the
/// multi-select per-band channel menu). `accent`-coloured labels read as
/// channel-specific (FL/FR), neutral as "all".
pub(crate) fn tag_chip(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    text: &str,
    text_color: Color32,
) -> egui::Response {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let border = if resp.hovered() {
        t.accent
    } else {
        lerp_color(t.well, text_color, 0.45)
    };
    ui.painter().rect_filled(rect, 4.0, t.well);
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    // Label centred, leaving room for a small caret on the right.
    let text_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 12.0, rect.max.y));
    ui.painter().text(
        text_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(T_VALUE),
        text_color,
    );
    let cx = rect.right() - 8.0;
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
    resp
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

/// An icon button that shows an `active` (accent-filled) state — for toggles like
/// per-band solo where the pressed state must read at a glance.
pub(crate) fn icon_btn_active(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    active: bool,
    tip: &str,
) -> bool {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let (bg, fg) = if resp.hovered() {
        (lerp_color(t.well, t.accent, 0.30), egui::Color32::WHITE)
    } else if active {
        (t.accent, egui::Color32::WHITE)
    } else {
        (t.well, t.text)
    };
    ui.painter().rect_filled(rect, 5.0, bg);
    let g = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(size * 0.64));
    icons::draw(ui.painter(), icon, g, fg);
    let clicked = resp.clicked();
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

/// A larger bespoke button with an explicit minimum size and font — for hero
/// actions like the "Start daemon" call-to-action on the disconnected screen.
/// `accent` fills it; otherwise it sits in a `well`. Returns true on click.
pub(crate) fn button_sized(
    ui: &mut egui::Ui,
    label: &str,
    accent: bool,
    enabled: bool,
    min_size: egui::Vec2,
    font_size: f32,
) -> bool {
    let t = tokens(ui);
    let font = egui::FontId::proportional(font_size);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), t.text);
    let w = (galley.size().x + 28.0).max(min_size.x);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, min_size.y), egui::Sense::click());
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
    ui.painter().rect_filled(rect, R_CTRL, bg);
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, label, font, fg);
    enabled && resp.clicked()
}

/// A bespoke button filled with an explicit colour (e.g. the `cut` red for a
/// destructive confirm), sized to its label. White label; brightens on hover.
pub(crate) fn button_filled(ui: &mut egui::Ui, label: &str, fill: Color32, enabled: bool) -> bool {
    let font = egui::FontId::proportional(T_BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE);
    let w = galley.size().x + 24.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CTRL_H), egui::Sense::click());
    let bg = if !enabled {
        tokens(ui).well
    } else if resp.hovered() {
        fill
    } else {
        fill.gamma_multiply(0.88)
    };
    let fg = if enabled {
        Color32::WHITE
    } else {
        tokens(ui).dim
    };
    ui.painter().rect_filled(rect, R_CTRL, bg);
    ui.painter()
        .text(rect.center(), egui::Align2::CENTER_CENTER, label, font, fg);
    enabled && resp.clicked()
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
    menu_button_ex(ui, label, label_color, popup_id, true, "", add);
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

/// A full-width selectable list row (the kit replacement for
/// `egui::selectable_label` in file browsers / catalogs): hover highlight, an
/// accent wash + accent text when `selected`. Returns its `Response` so callers
/// handle click / double-click themselves.
pub(crate) fn list_row(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let t = tokens(ui);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, 4.0, t.accent.gamma_multiply(0.20));
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, t.accent.gamma_multiply(0.10));
    }
    fade_text(
        ui,
        egui::Rect::from_min_max(egui::pos2(rect.left() + 7.0, rect.top()), rect.max),
        label,
        egui::FontId::proportional(T_BODY),
        if selected { t.accent } else { t.text },
        ui.visuals().window_fill,
    );
    resp
}

/// A flat inset container (the kit replacement for `egui::Frame::group`): a
/// `well` surface with a hairline border, for list/scroll panels inside dialogs.
pub(crate) fn well_frame<R>(
    ui: &mut egui::Ui,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let t = tokens(ui);
    egui::Frame::default()
        .fill(t.well)
        .stroke(egui::Stroke::new(1.0, t.line))
        .corner_radius(egui::CornerRadius::same(R_CTRL as u8))
        .inner_margin(egui::Margin::same(SP_XS as i8))
        .show(ui, add)
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
