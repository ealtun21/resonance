//! The compact "Reference" bar under the FR graph, plus the inline target
//! customizer and the Auto-EQ action. Off by default (master toggle) so the
//! graph stays pure EQ until the user opts in — keeping the main UI clean.

use crate::app::GuiApp;
use crate::state::Snapshot;
use crate::ui::icons::Icon;
use crate::ui::kit;
use crate::ui::widgets::{dialog_window, ellipsize};
use eframe::egui;
use resonance_autoeq::{BandKind, Smoothing};
use resonance_ipc::{BandState, BandType};
use resonance_reference::download::{DlCmd, ModelEntry, TargetEntry};
use resonance_reference::reference::{Channel, ManageTab};

/// The three logical sections of the reference bar, drawn left→right with a
/// hairline between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Targets,
    Measurements,
    AutoEq,
}

impl Section {
    fn caption(self) -> &'static str {
        match self {
            Section::Targets => "Targets",
            Section::Measurements => "Measurements",
            Section::AutoEq => "Auto-EQ",
        }
    }
}

/// Every reference-bar control.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RefCtl {
    TargetDd,
    Customize,
    Manage,
    MeasChip,
    Clear,
    Channel,
    MeasFile,
    ToTarget,
    Raw,
    Bounds,
    Normalize,
    AutoEq,
}

/// `(control, section, core, drop_priority)` in left→right display order. `core`
/// controls never collapse (the target picker + measurement chip — the minimum
/// to be useful); among the rest, the LOWEST `drop_priority` collapses into the
/// ☰ menu first as the bar narrows, so the view toggles go before Auto-EQ/Clear.
const REF_LAYOUT: [(RefCtl, Section, bool, u8); 12] = [
    (RefCtl::TargetDd, Section::Targets, true, 0),
    (RefCtl::Customize, Section::Targets, false, 8),
    (RefCtl::Manage, Section::Targets, false, 7),
    (RefCtl::MeasChip, Section::Measurements, true, 0),
    (RefCtl::Clear, Section::Measurements, false, 9),
    (RefCtl::Channel, Section::Measurements, false, 6),
    (RefCtl::MeasFile, Section::Measurements, false, 5),
    (RefCtl::ToTarget, Section::Measurements, false, 4),
    (RefCtl::Raw, Section::Measurements, false, 3),
    (RefCtl::Bounds, Section::Measurements, false, 2),
    (RefCtl::Normalize, Section::Measurements, false, 1),
    (RefCtl::AutoEq, Section::AutoEq, false, 10),
];

const SECTIONS: [Section; 3] = [Section::Targets, Section::Measurements, Section::AutoEq];

impl GuiApp {
    /// Control strip under the FR graph: three sections (Targets · Measurements ·
    /// Auto-EQ) separated by hairlines. A single non-wrapping row that collapses
    /// its secondary controls into a ☰ menu (lowest priority first) only when it
    /// runs out of width — so the ☰ appears solely when something didn't fit.
    pub(crate) fn reference_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(kit::SP_XS);

        // Disabled: just the master toggle pill + a one-line hint.
        if !self.reference.enabled {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                if kit::pill_check(
                    ui,
                    "Reference",
                    false,
                    "overlay a target + measurement to EQ by eye",
                ) {
                    self.reference.enabled = true;
                }
                ui.label(
                    egui::RichText::new("overlay a target + measurement to EQ by eye")
                        .size(kit::T_CAPTION)
                        .weak(),
                );
            });
            return;
        }

        let has_meas = self.reference.measurement.is_some();
        let has_stereo = matches!(&self.reference.measurement_lr, Some((_, Some(_))));
        let busy = self.autoeq_busy;
        let can_auto = has_meas && self.reference.target.is_some() && !busy;
        let gap = kit::SP_S;

        // Controls present right now (some need a measurement), in display order.
        let present: Vec<(RefCtl, Section, bool, u8)> = REF_LAYOUT
            .into_iter()
            .filter(|&(c, ..)| ctl_present(c, has_meas, has_stereo))
            .collect();

        // Width fit: start with everything inline, then drop the lowest-priority
        // non-core control until the row fits — accounting for the inter-section
        // hairlines and (only when something has collapsed) the ☰ button.
        // The master Reference pill (check + label) is always present; its width
        // is the leading inset the section fit reasons about.
        let leading = 42.0 + kit::text_width(ui, kit::T_VALUE, "Reference") + gap;
        let sep_unit = 1.0 + 2.0 * gap;
        let overflow_w = 28.0 + gap;
        let avail = ui.available_width();
        let mut inline: Vec<RefCtl> = present.iter().map(|&(c, ..)| c).collect();
        loop {
            let n_sections = SECTIONS
                .iter()
                .filter(|&&s| {
                    present
                        .iter()
                        .any(|&(c, sec, ..)| sec == s && inline.contains(&c))
                })
                .count();
            let collapsed_any = inline.len() < present.len();
            let used: f32 = inline
                .iter()
                .map(|&c| self.ref_ctl_width(ui, c, busy) + gap)
                .sum::<f32>()
                + leading
                + n_sections.saturating_sub(1) as f32 * sep_unit
                + if collapsed_any { overflow_w } else { 0.0 };
            if used <= avail {
                break;
            }
            // Drop the lowest-priority collapsible control still inline.
            let drop = present
                .iter()
                .filter(|&&(c, _, core, _)| !core && inline.contains(&c))
                .min_by_key(|&&(_, _, _, p)| p)
                .map(|&(c, ..)| c);
            match drop {
                Some(c) => inline.retain(|&x| x != c),
                None => break,
            }
        }
        let collapsed: Vec<(RefCtl, Section)> = present
            .iter()
            .filter(|&&(c, ..)| !inline.contains(&c))
            .map(|&(c, s, ..)| (c, s))
            .collect();

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if kit::pill_check(ui, "Reference", true, "turn off the reference overlay") {
                self.reference.enabled = false;
            }

            // Sections, each preceded by a hairline once anything's been drawn.
            let mut drawn = false;
            for section in SECTIONS {
                let items: Vec<RefCtl> = present
                    .iter()
                    .filter(|&&(c, s, ..)| s == section && inline.contains(&c))
                    .map(|&(c, ..)| c)
                    .collect();
                if items.is_empty() {
                    continue;
                }
                if drawn {
                    self.tb_sep(ui);
                }
                for c in items {
                    self.ref_ctl_inline(ui, c, can_auto, busy);
                }
                drawn = true;
            }

            // ☰ overflow — only when something collapsed.
            if !collapsed.is_empty() {
                if drawn {
                    self.tb_sep(ui);
                }
                self.ref_overflow(ui, &collapsed, can_auto, busy);
            }
        });
    }

    /// Inline width a control occupies (used by the collapse fit). Pills: ~22px
    /// padding + 20px per leading check/icon + the label width.
    fn ref_ctl_width(&self, ui: &egui::Ui, c: RefCtl, busy: bool) -> f32 {
        let check = |s: &str| 22.0 + 20.0 + kit::text_width(ui, kit::T_VALUE, s);
        let icon_label = |s: &str| 22.0 + 20.0 + kit::text_width(ui, kit::T_VALUE, s);
        let icon_only = 22.0 + 14.0;
        let label_only = |s: &str| 22.0 + kit::text_width(ui, kit::T_VALUE, s);
        match c {
            RefCtl::TargetDd => {
                kit::text_width(ui, kit::T_VALUE, "Target") + 2.0 + kit::SP_S + 148.0
            }
            RefCtl::Customize => icon_label("Customize"),
            RefCtl::Manage => icon_only,
            RefCtl::MeasChip => label_only(&self.meas_label()),
            RefCtl::Clear => icon_only,
            RefCtl::Channel => label_only(self.reference.channel.label()),
            RefCtl::MeasFile => icon_only,
            RefCtl::ToTarget => icon_only,
            RefCtl::Raw => check("Raw"),
            RefCtl::Bounds => check("Bounds"),
            RefCtl::Normalize => check("Normalize"),
            RefCtl::AutoEq => icon_label(if busy { "Auto-EQ…" } else { "Auto-EQ" }),
        }
    }

    /// The measurement chip's label: the loaded name, or a "load" prompt.
    fn meas_label(&self) -> String {
        if self.reference.measurement.is_some() {
            ellipsize(&self.reference.measurement_name, 20)
        } else {
            "Load measurement…".to_string()
        }
    }

    /// Render one control inline as a pill (mockup `.refbar`). Secondary actions
    /// are icon-only ghost pills with a hover tooltip; the view flags are
    /// check-pills; Auto-EQ is an accent pill.
    fn ref_ctl_inline(&mut self, ui: &mut egui::Ui, c: RefCtl, can_auto: bool, busy: bool) {
        match c {
            RefCtl::TargetDd => {
                // A rounded "Target" selector: an accent prefix label + the value
                // dropdown, sized to the pill row height (mockup `.ddl`).
                let accent = kit::tokens(ui).accent;
                let (lr, _) = ui.allocate_exact_size(
                    egui::vec2(
                        kit::text_width(ui, kit::T_VALUE, "Target") + 2.0,
                        kit::PILL_H,
                    ),
                    egui::Sense::hover(),
                );
                ui.painter().text(
                    egui::pos2(lr.left(), lr.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Target",
                    egui::FontId::proportional(kit::T_VALUE),
                    accent,
                );
                let opts = self.reference.target_options();
                let labels: Vec<&str> = opts.iter().map(|(n, _)| n.as_str()).collect();
                let cur = self.reference.target_label();
                if let Some(i) = kit::dropdown(
                    ui,
                    148.0,
                    kit::PILL_H,
                    ui.make_persistent_id("ref_target_dd"),
                    &cur,
                    &labels,
                ) {
                    self.reference.set_target(opts[i].1.clone());
                }
            }
            RefCtl::Customize => self.ref_customize_button(ui),
            RefCtl::Manage => {
                if kit::pill_icon(
                    ui,
                    Some(Icon::Download),
                    "",
                    false,
                    true,
                    true,
                    "Manage targets — add curves or measurements from squig.link, or remove them",
                ) {
                    self.open_manage();
                }
            }
            RefCtl::MeasChip => {
                let has_meas = self.reference.measurement.is_some();
                let tip = if has_meas {
                    "Measurement to overlay — click to pick another"
                } else {
                    "Pick a headphone/IEM measurement from squig.link to overlay"
                };
                let label = self.meas_label();
                if kit::pill_icon(ui, None, &label, false, false, true, tip) {
                    self.open_browser();
                }
            }
            RefCtl::Clear => {
                if kit::pill_icon(
                    ui,
                    Some(Icon::Close),
                    "",
                    false,
                    true,
                    true,
                    "Remove the loaded measurement",
                ) {
                    self.reference.clear_measurement();
                    self.touch_measurement();
                }
            }
            RefCtl::Channel => {
                if kit::pill_icon(
                    ui,
                    None,
                    self.reference.channel.label(),
                    false,
                    false,
                    true,
                    "Measurement channel: cycle L+R average / Left / Right",
                ) {
                    self.cycle_channel();
                }
            }
            RefCtl::ToTarget => {
                if kit::pill_icon(
                    ui,
                    Some(Icon::Save),
                    "",
                    false,
                    true,
                    true,
                    "Save this measurement as a target curve to EQ toward",
                ) {
                    self.meas_to_target();
                }
            }
            RefCtl::MeasFile => {
                if kit::pill_icon(
                    ui,
                    Some(Icon::Folder),
                    "",
                    false,
                    true,
                    true,
                    "Load a measurement from a local .txt/.csv file",
                ) {
                    self.open_curve_picker(false);
                }
            }
            RefCtl::Raw => {
                if kit::pill_check(
                    ui,
                    "Raw",
                    self.reference.show_measurement,
                    "Also draw the raw (un-EQ'd) measurement",
                ) {
                    self.reference.show_measurement = !self.reference.show_measurement;
                }
            }
            RefCtl::Bounds => {
                if kit::pill_check(
                    ui,
                    "Bounds",
                    self.reference.show_bounds,
                    "Shade the listener-preference tolerance band around the target \
                     (tight in the mids, wider in bass/treble) — keep the result inside it",
                ) {
                    self.reference.show_bounds = !self.reference.show_bounds;
                }
            }
            RefCtl::Normalize => {
                if kit::pill_check(
                    ui,
                    "Normalize",
                    self.reference.normalized,
                    "Flatten the target to 0 dB; show the EQ'd result as deviation",
                ) {
                    self.reference.normalized = !self.reference.normalized;
                }
            }
            RefCtl::AutoEq => {
                let label = if busy { "Auto-EQ…" } else { "Auto-EQ" };
                if kit::pill_icon(
                    ui,
                    Some(Icon::Wand),
                    label,
                    true,
                    false,
                    can_auto,
                    "Fit EQ bands so the EQ'd measurement matches the target (peqdb AutoEQ)",
                ) {
                    self.auto_eq(ui.ctx().clone());
                }
            }
        }
    }

    /// The ☰ overflow menu: the controls that didn't fit, grouped under their
    /// section caption. Stays open while toggling in-menu switches.
    fn ref_overflow(
        &mut self,
        ui: &mut egui::Ui,
        collapsed: &[(RefCtl, Section)],
        can_auto: bool,
        busy: bool,
    ) {
        kit::icon_menu_button(
            ui,
            Icon::Menu,
            egui::Id::new("ref_overflow_pop"),
            false,
            "More reference controls",
            |ui| {
                ui.set_min_width(230.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                for section in SECTIONS {
                    let items: Vec<RefCtl> = collapsed
                        .iter()
                        .filter(|&&(_, s)| s == section)
                        .map(|&(c, _)| c)
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    kit::menu_caption(ui, section.caption());
                    for c in items {
                        self.ref_ctl_menu(ui, c, can_auto, busy);
                    }
                }
            },
        );
    }

    /// Render one collapsed control as a menu row.
    fn ref_ctl_menu(&mut self, ui: &mut egui::Ui, c: RefCtl, can_auto: bool, busy: bool) {
        match c {
            // Cores never collapse, so they never reach the menu.
            RefCtl::TargetDd | RefCtl::MeasChip => {}
            RefCtl::Customize => {
                kit::menu_caption(ui, "Customize target");
                self.reference_customizer_body(ui);
            }
            RefCtl::Manage => {
                if kit::menu_item(ui, "Manage targets…", false) {
                    self.open_manage();
                }
            }
            RefCtl::Clear => {
                if kit::menu_item(ui, "Clear measurement", false) {
                    self.reference.clear_measurement();
                    self.touch_measurement();
                }
            }
            RefCtl::Channel => {
                let label = format!("Channel: {}", self.reference.channel.label());
                if kit::menu_item(ui, &label, false) {
                    self.cycle_channel();
                }
            }
            RefCtl::ToTarget => {
                if kit::menu_item(ui, "Save measurement as target", false) {
                    self.meas_to_target();
                }
            }
            RefCtl::MeasFile => {
                if kit::menu_item(ui, "Load measurement file…", false) {
                    self.open_curve_picker(false);
                }
            }
            RefCtl::Raw => {
                if kit::menu_item(ui, "Show raw measurement", self.reference.show_measurement) {
                    self.reference.show_measurement = !self.reference.show_measurement;
                }
            }
            RefCtl::Bounds => {
                if kit::menu_item(ui, "Preference bounds", self.reference.show_bounds) {
                    self.reference.show_bounds = !self.reference.show_bounds;
                }
            }
            RefCtl::Normalize => {
                if kit::menu_item(ui, "Normalize to target", self.reference.normalized) {
                    self.reference.normalized = !self.reference.normalized;
                }
            }
            RefCtl::AutoEq => {
                let label = if busy { "Auto-EQ…" } else { "Auto-EQ" };
                if kit::menu_item(ui, label, false) && can_auto {
                    self.auto_eq(ui.ctx().clone());
                }
            }
        }
    }

    fn open_browser(&mut self) {
        self.reference.show_browser = true;
        if self.catalog.is_none() && !self.dl_busy {
            let _ = self.dl_tx.send(DlCmd::Init);
        }
    }

    fn open_manage(&mut self) {
        self.reference.show_manage = true;
        if self.catalog.is_none() && !self.dl_busy {
            let _ = self.dl_tx.send(DlCmd::Init);
        }
    }

    fn cycle_channel(&mut self) {
        self.reference.channel = match self.reference.channel {
            Channel::Avg => Channel::Left,
            Channel::Left => Channel::Right,
            Channel::Right => Channel::Avg,
        };
        self.reference.rebuild_measurement();
    }

    fn meas_to_target(&mut self) {
        if let Some(m) = self.reference.measurement.clone() {
            let name = self.reference.measurement_name.clone();
            self.reference.save_target(&name, &m);
        }
    }

    /// The "Customize target" button: opens the target customizer in a popup
    /// anchored directly *below* the button (not at the panel's left edge), which
    /// stays open while you drag its sliders.
    fn ref_customize_button(&mut self, ui: &mut egui::Ui) {
        let id = ui.make_persistent_id("ref_customize_pop");
        // Dev/screenshot hook (`RESONANCE_OPEN=customize`): hold the customizer
        // popup open so the harness can capture it without a click.
        if self.open_customizer {
            egui::Popup::open_id(ui.ctx(), id);
        }
        kit::pill_popup(
            ui,
            Some(Icon::Sliders),
            "Customize",
            "Shape the selected target: tilt + bass / ear-gain / treble shelves (stacks on any target)",
            id,
            340.0,
            |ui| self.reference_customizer_body(ui),
        );
    }

    /// Paint the customizer's live FR thumbnail into a short deep well: a 0-dB
    /// baseline, the base target dashed, the adjusted target solid accent, a
    /// "Stacking on <target>" label, and 20 Hz / 20 k axis hints. Fixed ±15 dB so
    /// dragging a slider bends the curve instead of rescaling the frame.
    fn customizer_thumbnail(&self, ui: &mut egui::Ui) {
        let pal = self.palette;
        let t = kit::tokens(ui);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());

        let lmin = 20f64.log10();
        let lmax = 20_000f64.log10();
        let win = 15.0f64;
        let x_of = |f: f64| {
            let l = f.log10().clamp(lmin, lmax);
            rect.left() + ((l - lmin) / (lmax - lmin)) as f32 * rect.width()
        };
        let half = rect.height() * 0.5 - 4.0;
        let y_of = |db: f64| rect.center().y - (db.clamp(-win, win) / win) as f32 * half;

        let p = ui.painter_at(rect);
        p.rect_filled(rect, kit::R_CTRL, pal.graph_bg);
        p.rect_stroke(
            rect,
            kit::R_CTRL,
            egui::Stroke::new(1.0, t.line),
            egui::StrokeKind::Inside,
        );
        p.hline(rect.x_range(), y_of(0.0), egui::Stroke::new(1.0, t.faint));

        if self.reference.target.is_some() {
            if let Some(b) = self.reference.base_curve() {
                let path: Vec<egui::Pos2> = b
                    .points
                    .iter()
                    .map(|&(f, db)| egui::pos2(x_of(f), y_of(db)))
                    .collect();
                p.add(egui::Shape::dashed_line(
                    &path,
                    egui::Stroke::new(1.0, t.faint),
                    3.0,
                    3.0,
                ));
            }
            if let Some(adj) = &self.reference.target {
                let path: Vec<egui::Pos2> = adj
                    .points
                    .iter()
                    .map(|&(f, db)| egui::pos2(x_of(f), y_of(db)))
                    .collect();
                p.add(egui::Shape::line(path, egui::Stroke::new(1.5, pal.accent)));
            }
            // Context label on an opaque graph-bg scrim (covers the curve behind it).
            let label = format!("Stacking on {}", self.reference.target_label());
            let lab = elide(ui, &label, kit::T_CAPTION, rect.width() * 0.72);
            let lw = kit::text_width(ui, kit::T_CAPTION, &lab);
            let lr = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 6.0, rect.top() + 5.0),
                egui::vec2(lw + 10.0, 14.0),
            );
            p.rect_filled(lr, 3.0, pal.graph_bg);
            p.text(
                egui::pos2(lr.left() + 5.0, lr.center().y),
                egui::Align2::LEFT_CENTER,
                &lab,
                egui::FontId::proportional(kit::T_CAPTION),
                t.text,
            );
        } else {
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Select a target to customize",
                egui::FontId::proportional(kit::T_CAPTION),
                t.faint,
            );
        }

        let small = egui::FontId::proportional(9.0);
        p.text(
            egui::pos2(rect.left() + 5.0, rect.bottom() - 7.0),
            egui::Align2::LEFT_CENTER,
            "20 Hz",
            small.clone(),
            t.faint,
        );
        p.text(
            egui::pos2(rect.right() - 5.0, rect.bottom() - 7.0),
            egui::Align2::RIGHT_CENTER,
            "20 k",
            small,
            t.faint,
        );
    }

    /// The on-the-fly target customizer body (inside the anchored popup): tilt +
    /// bass / ear-gain / treble shelves stacked on whichever target is selected.
    /// Reset zeroes the adjustments; Save bakes the result into the library.
    fn reference_customizer_body(&mut self, ui: &mut egui::Ui) {
        kit::menu_caption(ui, "Customize target");
        ui.label(
            egui::RichText::new("Stacks on the selected target. Save bakes it into the library.")
                .size(kit::T_CAPTION)
                .color(kit::tokens(ui).faint),
        );

        // Live FR thumbnail: the adjusted target (solid accent) over the base
        // target (dashed) on a fixed ±15 dB window, so the abstract Tilt / Bass /
        // Ear / Treble knobs become visible. Zero new DSP — it paints the curve
        // points `rebuild_target()` already produced.
        self.customizer_thumbnail(ui);
        ui.add_space(kit::SP_S);

        let pal = self.palette;
        let mut changed = false;
        changed |= cust_slider(
            ui,
            &pal,
            "Tilt",
            &mut self.reference.adj_tilt,
            -2.0..=1.0,
            "dB/oct",
            2,
        );
        changed |= cust_slider(
            ui,
            &pal,
            "Bass",
            &mut self.reference.adj_bass,
            -12.0..=18.0,
            "dB",
            1,
        );
        changed |= cust_slider(
            ui,
            &pal,
            "Ear",
            &mut self.reference.adj_ear,
            -12.0..=12.0,
            "dB",
            1,
        );
        changed |= cust_slider(
            ui,
            &pal,
            "Treble",
            &mut self.reference.adj_treble,
            -12.0..=12.0,
            "dB",
            1,
        );

        ui.add_space(kit::SP_S);
        {
            let line = kit::tokens(ui).line;
            let (r, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
            ui.painter()
                .hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, line));
        }
        ui.add_space(kit::SP_S);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            if kit::button_tip(ui, "Reset", false, true, "Zero all four adjustments") {
                self.reference.adj_tilt = 0.0;
                self.reference.adj_bass = 0.0;
                self.reference.adj_ear = 0.0;
                self.reference.adj_treble = 0.0;
                changed = true;
            }
            kit::text_field(
                ui,
                140.0,
                ui.make_persistent_id("ref_target_name"),
                &mut self.reference.target_name,
                "saved name…",
                false,
            );
            let savable = self.reference.target.is_some();
            if kit::button_tip(
                ui,
                "Save",
                true,
                savable,
                "Bake the customized curve into the target library",
            ) {
                if let Some(curve) = self.reference.target.clone() {
                    let name = if self.reference.target_name.trim().is_empty() {
                        format!("{} (custom)", self.reference.target_label())
                    } else {
                        self.reference.target_name.clone()
                    };
                    self.reference.save_target(&name, &curve);
                    self.reference.target_name.clear();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            if self.reference.active_target_removable()
                && kit::button_tip(
                    ui,
                    "Remove this target",
                    false,
                    true,
                    "Delete (user curve) or hide (built-in) the selected target",
                )
            {
                let label = self.reference.target_label();
                self.reference.remove_target_label(&label);
            }
            if kit::button_tip(
                ui,
                "Import file…",
                false,
                true,
                "Import a target curve from a local .txt/.csv file",
            ) {
                self.open_curve_picker(true);
            }
        });

        if changed {
            self.reference.rebuild_target();
        }
    }

    /// The "Manage targets" dialog: add target curves or measurements from
    /// squig.link (each a separate search), or remove targets / reset to
    /// defaults. The selector only shows the curated library; this is where the
    /// library is grown and pruned.
    pub(crate) fn manage_dialog(&mut self, ctx: &egui::Context) {
        if !self.reference.show_manage {
            return;
        }
        if self.catalog.is_none() && !self.dl_busy {
            let _ = self.dl_tx.send(DlCmd::Init);
        }

        let mut open = true;
        let mut close = false;
        let mut fetch_target: Option<TargetEntry> = None;
        let mut add_meas: Option<ModelEntry> = None;
        let mut remove_label: Option<String> = None;
        let mut do_reset = false;
        let mut refresh = false;
        let mut toggle: Option<(String, bool)> = None;
        let mut set_all: Option<bool> = None;
        let mut set_default = false;

        // Snapshot the library (owned) before borrowing `reference` mutably.
        let lib = self.reference.library_entries();
        let hidden = self.reference.hidden_count();
        let catalog = self.catalog.as_ref();
        let dl_status = self.dl_status.as_str();
        let dl_busy = self.dl_busy;
        let reference = &mut self.reference;

        let def_h = (ctx.content_rect().height() * 0.7).clamp(320.0, 640.0);
        dialog_window(ctx, "Manage targets")
            .id(egui::Id::new("resonance_manage_dialog"))
            .default_height(def_h)
            .open(&mut open)
            .show(ctx, |ui| {
                // Tabs + search (top, fixed).
                egui::Panel::top("manage_top").show_inside(ui, |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = kit::SP_S;
                        let tabs = [
                            (
                                ManageTab::Targets,
                                "Target curves",
                                "Browse squig.link target curves to add to your library",
                            ),
                            (
                                ManageTab::Measurements,
                                "Measurements",
                                "Add a headphone/IEM measurement as a target (L+R averaged)",
                            ),
                            (
                                ManageTab::Yours,
                                "Your targets",
                                "Your current library — remove targets or reset to defaults",
                            ),
                        ];
                        for (t, label, tip) in tabs {
                            if kit::button_tip(ui, label, reference.manage_tab == t, true, tip) {
                                reference.manage_tab = t;
                            }
                        }
                    });
                    if reference.manage_tab != ManageTab::Yours {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("search").size(kit::T_CAPTION).weak());
                            let (id, buf, hint) = if reference.manage_tab == ManageTab::Targets {
                                (
                                    "manage_tq",
                                    &mut reference.manage_tquery,
                                    "target curve name…",
                                )
                            } else {
                                (
                                    "manage_mq",
                                    &mut reference.manage_mquery,
                                    "headphone or IEM…",
                                )
                            };
                            kit::text_field(ui, 240.0, egui::Id::new(id), buf, hint, false);
                            if kit::icon_text_btn(
                                ui,
                                Icon::Refresh,
                                "Refresh",
                                false,
                                true,
                                "Re-fetch source indexes from squig.link",
                            ) {
                                refresh = true;
                            }
                            if dl_busy {
                                ui.label(egui::RichText::new("loading…").weak());
                            }
                        });
                    }
                    if !dl_status.is_empty() {
                        ui.label(egui::RichText::new(dl_status).size(kit::T_CAPTION).weak());
                    }
                    ui.add_space(2.0);
                });

                // Sources + Close (bottom, fixed) — same federation toggles as the
                // measurement browser; enabling more sites surfaces more targets.
                egui::Panel::bottom("manage_bottom").show_inside(ui, |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("sources").size(kit::T_CAPTION).weak());
                        if kit::button_tip(ui, "All", false, true, "Enable every squig.link source")
                        {
                            set_all = Some(true);
                        }
                        if kit::button_tip(ui, "None", false, true, "Disable every source") {
                            set_all = Some(false);
                        }
                        if kit::button_tip(
                            ui,
                            "Default",
                            false,
                            true,
                            "Restore the built-in default source selection",
                        ) {
                            set_default = true;
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("manage_sources")
                        .max_height(64.0)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(kit::SP_XS, kit::SP_XS);
                                if let Some(cat) = catalog {
                                    for s in &cat.sources {
                                        let lbl = if s.enabled && !s.loaded {
                                            format!("{}…", s.name)
                                        } else {
                                            s.name.clone()
                                        };
                                        if kit::button_tip(
                                            ui,
                                            &lbl,
                                            s.enabled,
                                            true,
                                            "Toggle this squig.link source on/off",
                                        ) {
                                            toggle = Some((s.id.clone(), !s.enabled));
                                        }
                                    }
                                }
                            });
                        });
                    ui.separator();
                    if kit::button_tip(ui, "Close", false, true, "Close this dialog") {
                        close = true;
                    }
                    ui.add_space(2.0);
                });

                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("manage_list")
                        .auto_shrink([false, false])
                        .show(ui, |ui| match reference.manage_tab {
                            ManageTab::Targets => {
                                let q = reference.manage_tquery.to_lowercase();
                                let Some(cat) = catalog else {
                                    ui.weak("loading catalog…");
                                    return;
                                };
                                let mut shown = 0usize;
                                for t in &cat.targets {
                                    if !q.is_empty() && !t.name.to_lowercase().contains(&q) {
                                        continue;
                                    }
                                    shown += 1;
                                    if shown > 400 {
                                        continue;
                                    }
                                    ui.horizontal(|ui| {
                                        if kit::icon_text_btn(
                                            ui,
                                            Icon::Plus,
                                            "Add",
                                            false,
                                            true,
                                            "Add this target curve to your library",
                                        ) {
                                            fetch_target = Some(t.clone());
                                        }
                                        ui.label(format!("{}   ·  {}", t.name, t.source));
                                    });
                                }
                                if shown == 0 {
                                    ui.weak(
                                        "no target curves — enable more sources below, or Refresh",
                                    );
                                }
                            }
                            ManageTab::Measurements => {
                                let q = reference.manage_mquery.to_lowercase();
                                let Some(cat) = catalog else {
                                    ui.weak("loading catalog…");
                                    return;
                                };
                                let mut shown = 0usize;
                                for m in &cat.models {
                                    if !q.is_empty() && !m.display.to_lowercase().contains(&q) {
                                        continue;
                                    }
                                    shown += 1;
                                    if shown > 400 {
                                        continue;
                                    }
                                    ui.horizontal(|ui| {
                                        if kit::icon_text_btn(
                                            ui,
                                            Icon::Plus,
                                            "Add",
                                            false,
                                            true,
                                            "Add this measurement (L+R averaged) as a target",
                                        ) {
                                            add_meas = Some(m.clone());
                                        }
                                        ui.label(format!(
                                            "{}  ·  {} · {}",
                                            m.display, m.source, m.kind
                                        ));
                                    });
                                }
                                if shown == 0 {
                                    ui.weak("type to search, or enable more sources below");
                                }
                            }
                            ManageTab::Yours => {
                                ui.horizontal(|ui| {
                                    let label = if hidden == 0 {
                                        "Reset to defaults".to_string()
                                    } else {
                                        format!("Reset to defaults ({hidden} hidden)")
                                    };
                                    if kit::button_tip(
                                        ui,
                                        &label,
                                        false,
                                        true,
                                        "Un-hide every built-in / generated target",
                                    ) {
                                        do_reset = true;
                                    }
                                });
                                ui.add_space(kit::SP_XS);
                                if lib.is_empty() {
                                    ui.weak("no targets — add some from the other tabs");
                                }
                                for e in &lib {
                                    ui.horizontal(|ui| {
                                        let tip = if e.builtin {
                                            "Hide this built-in / generated target"
                                        } else {
                                            "Delete this user target file"
                                        };
                                        if kit::icon_btn(ui, Icon::Close, 22.0, tip) {
                                            remove_label = Some(e.label.clone());
                                        }
                                        let suffix = if e.builtin { "" } else { "  (added)" };
                                        ui.label(format!("{}{}", e.label, suffix));
                                    });
                                }
                            }
                        });
                });
            });

        // Apply collected actions once the borrows above are released.
        if let Some(t) = fetch_target {
            let _ = self.dl_tx.send(DlCmd::FetchTarget(t));
        }
        if let Some(m) = add_meas {
            let _ = self.dl_tx.send(DlCmd::AddMeasurementTarget(m));
        }
        if let Some(label) = remove_label {
            self.reference.remove_target_label(&label);
        }
        if do_reset {
            self.reference.reset_targets_to_defaults();
        }
        if refresh {
            let _ = self.dl_tx.send(DlCmd::Refresh);
        }
        if let Some((id, on)) = toggle {
            let _ = self.dl_tx.send(DlCmd::SetEnabled(id, on));
        }
        if let Some(on) = set_all {
            let _ = self.dl_tx.send(DlCmd::SetAll(on));
        }
        if set_default {
            let _ = self.dl_tx.send(DlCmd::SetDefault);
        }
        if !open || close {
            self.reference.show_manage = false;
        }
    }

    /// Auto-EQ: fit a parametric bank (peqdb's AutoEQ, ported to Rust in
    /// `resonance-autoeq`) so the measurement, once EQ'd, matches the target.
    /// The 3000-step optimize runs on a background thread; [`Self::pump_autoeq`]
    /// applies the result (with a clip-safe headroom preamp) when it lands.
    fn auto_eq(&mut self, ctx: egui::Context) {
        if self.autoeq_busy {
            return;
        }
        let Some(st) = self.state.clone() else {
            return;
        };
        let (Some(meas), Some(tgt)) = (
            self.reference.measurement.clone(),
            self.reference.target.clone(),
        ) else {
            return;
        };
        // Sample both curves onto AutoEQ's fixed log grid (dB).
        let f = resonance_autoeq::log_freqs();
        let target: Vec<f32> = f.iter().map(|&hz| tgt.interp(hz as f64) as f32).collect();
        let measured: Vec<f32> = f.iter().map(|&hz| meas.interp(hz as f64) as f32).collect();
        let smoothing = if self.reference.measurement_iem {
            Smoothing::InEar
        } else {
            Smoothing::OverEar
        };
        let snapshot = Snapshot {
            preamp_db: st.preamp_db,
            enabled: st.enabled,
            bands: st.bands.clone(),
            effects: st.effects.clone(),
        };
        let tx = self.autoeq_tx.clone();
        self.autoeq_busy = true;
        self.set_status("Auto-EQ: fitting…");
        std::thread::Builder::new()
            .name("resonance-autoeq".into())
            .spawn(move || {
                let res = resonance_autoeq::run(&target, &measured, 10, smoothing, 3000);
                let bands: Vec<BandState> = res
                    .filters
                    .iter()
                    .map(|fl| BandState {
                        band_type: match fl.kind {
                            BandKind::Peak => BandType::Peaking,
                            BandKind::LowShelf => BandType::LowShelf,
                            BandKind::HighShelf => BandType::HighShelf,
                        },
                        freq: fl.freq,
                        gain_db: fl.gain_db,
                        q: fl.q,
                        enabled: true,
                        channels: resonance_ipc::ChannelMask::ALL,
                    })
                    .collect();
                let _ = tx.send(crate::app::AutoEqOutcome {
                    snapshot,
                    preamp_db: res.preamp_db,
                    bands,
                });
                ctx.request_repaint();
            })
            .ok();
    }

    /// The "Browse measurements" dialog: search the squig.link catalog, toggle
    /// sources, refresh, and load a model's curves as the active measurement.
    pub(crate) fn browse_dialog(&mut self, ctx: &egui::Context) {
        if !self.reference.show_browser {
            return;
        }
        // Build the catalog from cache the first time the dialog opens.
        if self.catalog.is_none() && !self.dl_busy {
            let _ = self.dl_tx.send(DlCmd::Init);
        }

        let mut open = true;
        let mut close = false;
        let mut to_fetch: Option<ModelEntry> = None;
        let mut refresh = false;
        let mut toggle: Option<(String, bool)> = None;
        let mut set_all: Option<bool> = None;
        let mut set_default = false;

        // Disjoint field borrows so the closure can read the catalog and edit the
        // query without going through `self`.
        let catalog = self.catalog.as_ref();
        let query = &mut self.reference.browse_query;
        let dl_status = self.dl_status.as_str();
        let dl_busy = self.dl_busy;
        // Snapshot the filter text (1-frame lag while typing is fine) so the
        // central panel needn't re-borrow `query`.
        let q = query.to_lowercase();

        // A concrete default height so the central list has room; resizable from
        // there. (Panels fill it without the window auto-growing.)
        let def_h = (ctx.content_rect().height() * 0.7).clamp(320.0, 640.0);
        dialog_window(ctx, "Browse measurements")
            .id(egui::Id::new("resonance_browse_dialog"))
            .default_height(def_h)
            .open(&mut open)
            .show(ctx, |ui| {
                // Search row (top). Top + bottom panels fix the window's chrome so
                // the model list fills the middle without the window auto-growing.
                egui::Panel::top("browse_top").show_inside(ui, |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("search").size(kit::T_CAPTION).weak());
                        kit::text_field(
                            ui,
                            220.0,
                            ui.make_persistent_id("browse_query"),
                            query,
                            "headphone or IEM name…",
                            false,
                        );
                        if kit::icon_text_btn(
                            ui,
                            Icon::Refresh,
                            "Refresh",
                            false,
                            true,
                            "Re-fetch source indexes from squig.link",
                        ) {
                            refresh = true;
                        }
                        if dl_busy {
                            ui.label(egui::RichText::new("loading…").weak());
                        }
                    });
                    if !dl_status.is_empty() {
                        ui.label(egui::RichText::new(dl_status).size(kit::T_CAPTION).weak());
                    }
                    ui.add_space(2.0);
                });

                // Sources + Close (bottom, fixed).
                egui::Panel::bottom("browse_bottom").show_inside(ui, |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("sources").size(kit::T_CAPTION).weak());
                        if kit::button_tip(ui, "All", false, true, "Enable every squig.link source")
                        {
                            set_all = Some(true);
                        }
                        if kit::button_tip(ui, "None", false, true, "Disable every source") {
                            set_all = Some(false);
                        }
                        if kit::button_tip(
                            ui,
                            "Default",
                            false,
                            true,
                            "Restore the built-in default source selection",
                        ) {
                            set_default = true;
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("browse_sources")
                        .max_height(80.0)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(kit::SP_XS, kit::SP_XS);
                                if let Some(cat) = catalog {
                                    for s in &cat.sources {
                                        let lbl = if s.enabled && !s.loaded {
                                            format!("{}…", s.name)
                                        } else {
                                            s.name.clone()
                                        };
                                        if kit::button_tip(
                                            ui,
                                            &lbl,
                                            s.enabled,
                                            true,
                                            "Toggle this squig.link source on/off",
                                        ) {
                                            toggle = Some((s.id.clone(), !s.enabled));
                                        }
                                    }
                                }
                            });
                        });
                    ui.separator();
                    if kit::button_tip(ui, "Close", false, true, "Close this dialog") {
                        close = true;
                    }
                    ui.add_space(2.0);
                });

                // Model list fills the central area (and grows when the window is
                // dragged taller); capped to 300 visible rows.
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let matches =
                        |m: &ModelEntry| q.is_empty() || m.display.to_lowercase().contains(&q);
                    egui::ScrollArea::vertical()
                        .id_salt("browse_models")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let Some(cat) = catalog else {
                                ui.weak("loading catalog…");
                                return;
                            };
                            if cat.models.is_empty() {
                                ui.weak("no models — enable a source below, or Refresh");
                            }
                            let mut shown = 0usize;
                            for m in cat.models.iter().filter(|m| matches(m)) {
                                shown += 1;
                                if shown > 300 {
                                    continue;
                                }
                                let label = format!("{}   ·  {} · {}", m.display, m.source, m.kind);
                                if kit::list_row(ui, false, &label).clicked() {
                                    to_fetch = Some(m.clone());
                                }
                            }
                            let total = cat.models.iter().filter(|m| matches(m)).count();
                            if total > 300 {
                                ui.weak(format!("+{} more — refine your search", total - 300));
                            }
                        });
                });
            });

        // Apply collected actions once the borrows above are released.
        if let Some(m) = to_fetch {
            let _ = self.dl_tx.send(DlCmd::Fetch(m));
        }
        if refresh {
            let _ = self.dl_tx.send(DlCmd::Refresh);
        }
        if let Some((id, on)) = toggle {
            let _ = self.dl_tx.send(DlCmd::SetEnabled(id, on));
        }
        if let Some(on) = set_all {
            let _ = self.dl_tx.send(DlCmd::SetAll(on));
        }
        if set_default {
            let _ = self.dl_tx.send(DlCmd::SetDefault);
        }
        if !open || close {
            self.reference.show_browser = false;
        }
    }

    /// Open the local-file picker (for a measurement, or `as_target` to import a
    /// target curve), starting in the user curve dir.
    fn open_curve_picker(&mut self, as_target: bool) {
        let dir = resonance_ipc::paths::user_curve_dir();
        let _ = std::fs::create_dir_all(&dir);
        let start = if dir.is_dir() {
            dir
        } else {
            crate::browser::home_dir()
        };
        self.dialog = crate::state::Dialog::ImportCurve {
            browser: crate::browser::Browser::new(start, true),
            as_target,
        };
    }

    /// File picker for a local curve: load it as the measurement, or import it
    /// into the target library.
    pub(crate) fn curve_picker_dialog(&mut self, ctx: &egui::Context) {
        let crate::state::Dialog::ImportCurve { browser, as_target } = &mut self.dialog else {
            return;
        };
        let as_target = *as_target;
        let title = if as_target {
            "Import target curve"
        } else {
            "Load measurement file"
        };

        let mut open = true;
        let mut close = false;
        let mut picked: Option<String> = None;
        let vh = ctx.content_rect().height();
        let list_h = (vh * 0.5).clamp(140.0, 360.0);

        dialog_window(ctx, title)
            .id(egui::Id::new("resonance_curve_picker"))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if kit::icon_btn(ui, Icon::Up, kit::CTRL_H, "Up to parent folder") {
                        browser.parent();
                    }
                    if kit::icon_btn(ui, Icon::Home, kit::CTRL_H, "Home folder") {
                        browser.navigate(crate::browser::home_dir());
                    }
                    if kit::button_tip(
                        ui,
                        "Curves",
                        false,
                        true,
                        "Go to the Resonance curves folder",
                    ) {
                        browser.navigate(resonance_ipc::paths::user_curve_dir());
                    }
                });
                ui.label(
                    egui::RichText::new(browser.cwd.display().to_string())
                        .size(kit::T_CAPTION)
                        .weak(),
                );
                ui.separator();
                kit::well_frame(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("curve_picker_list")
                        .auto_shrink([false, false])
                        .min_scrolled_height(list_h)
                        .max_height(list_h)
                        .show(ui, |ui| {
                            let mut do_select = None;
                            let mut do_activate = None;
                            for (i, it) in browser.entries.iter().enumerate() {
                                let glyph = if it.is_dir { "▸" } else { "·" };
                                let resp = kit::list_row(
                                    ui,
                                    i == browser.cursor,
                                    &format!("{glyph}  {}", it.name),
                                );
                                if resp.clicked() {
                                    do_select = Some(i);
                                }
                                if resp.double_clicked() {
                                    do_activate = Some(i);
                                }
                            }
                            if let Some(i) = do_select {
                                browser.select(i);
                            }
                            if let Some(i) = do_activate {
                                if let Some(p) = browser.activate(i) {
                                    picked = Some(p);
                                }
                            }
                        });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let is_file = browser.selected().map(|it| !it.is_dir).unwrap_or(false);
                    if kit::button(ui, "Load", true, is_file) {
                        if let Some(p) = browser.activate(browser.cursor) {
                            picked = Some(p);
                        }
                    }
                    if kit::button(ui, "Cancel", false, true) {
                        close = true;
                    }
                });
            });

        if let Some(p) = picked {
            let path = std::path::PathBuf::from(p);
            let ok = if as_target {
                self.reference.import_target_file(&path)
            } else {
                self.reference.load_measurement_file(&path)
            };
            if ok {
                self.reference.enabled = true;
                if !as_target {
                    // A new measurement while a profile is loaded → prompt re-save.
                    self.touch_measurement();
                }
            } else {
                self.set_status("couldn't parse that curve file");
            }
            close = true;
        }
        if !open || close {
            self.dialog = crate::state::Dialog::None;
        }
    }
}

/// Whether a control applies right now (most measurement controls need a loaded
/// measurement; the channel cycle additionally needs a stereo one).
fn ctl_present(c: RefCtl, has_meas: bool, has_stereo: bool) -> bool {
    match c {
        RefCtl::TargetDd
        | RefCtl::Customize
        | RefCtl::Manage
        | RefCtl::MeasChip
        | RefCtl::MeasFile
        | RefCtl::AutoEq => true,
        RefCtl::Clear | RefCtl::ToTarget | RefCtl::Raw | RefCtl::Bounds | RefCtl::Normalize => {
            has_meas
        }
        RefCtl::Channel => has_meas && has_stereo,
    }
}

/// A labelled customizer slider with a value chip. Returns true on change.
/// A center-zero customizer slider: label · divergence track (the fill grows out
/// of the param's *true* 0 mark — green above 0, red below) · colour-signed value
/// chip, with a min / 0 / max scale strip aligned beneath the track. Double-click
/// the track to zero it. Returns true while the value changes.
fn cust_slider(
    ui: &mut egui::Ui,
    pal: &crate::theme::Palette,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    unit: &str,
    decimals: usize,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let label_w = 46.0;
    let chip_w = if unit.len() > 3 { 96.0 } else { 64.0 };
    let gap = kit::SP_S;
    let pad = 7.0;
    let row_h = 20.0;
    let sw = (ui.available_width() - label_w - chip_w - gap * 2.0).max(60.0);
    // The 0 mark sits at the value's true fraction — NOT the geometric centre, so
    // the asymmetric ranges (Tilt −2..+1, Bass −12..+18) read honestly.
    let zero_frac = ((0.0 - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    let t = kit::tokens(ui);
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.set_min_height(row_h);
        ui.spacing_mut().item_spacing.x = gap;
        let (lr, _) = ui.allocate_exact_size(egui::vec2(label_w, row_h), egui::Sense::hover());
        let (track, resp) =
            ui.allocate_exact_size(egui::vec2(sw, row_h), egui::Sense::click_and_drag());
        let (cr, _) = ui.allocate_exact_size(egui::vec2(chip_w, row_h), egui::Sense::hover());

        let x0 = track.left() + pad;
        let x1 = track.right() - pad;
        let tw = (x1 - x0).max(1.0);
        let cy = track.center().y;

        if resp.double_clicked() {
            if (*value).abs() > f64::EPSILON {
                *value = 0.0;
                changed = true;
            }
        } else if resp.dragged() || resp.clicked() {
            if let Some(pp) = resp.interact_pointer_pos() {
                let f = ((pp.x - x0) / tw).clamp(0.0, 1.0) as f64;
                let nv = lo + f * (hi - lo);
                if (nv - *value).abs() > f64::EPSILON {
                    *value = nv;
                    changed = true;
                }
            }
        }

        let frac = ((*value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
        let hx = x0 + frac * tw;
        let zx = x0 + zero_frac * tw;
        let signed = |up, dn, neu| {
            if *value > 0.05 {
                up
            } else if *value < -0.05 {
                dn
            } else {
                neu
            }
        };
        let fill = signed(pal.boost, pal.cut, t.faint);

        let p = ui.painter();
        p.text(
            egui::pos2(lr.left(), lr.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(kit::T_BODY),
            t.text,
        );
        p.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, cy - 2.0), egui::pos2(x1, cy + 2.0)),
            2.0,
            t.well,
        );
        let (fa, fb) = if hx >= zx { (zx, hx) } else { (hx, zx) };
        p.rect_filled(
            egui::Rect::from_min_max(egui::pos2(fa, cy - 2.0), egui::pos2(fb, cy + 2.0)),
            2.0,
            fill,
        );
        p.line_segment(
            [egui::pos2(zx, cy - 5.0), egui::pos2(zx, cy + 5.0)],
            egui::Stroke::new(1.0, t.line),
        );
        let hr = if resp.hovered() || resp.dragged() {
            7.0
        } else {
            6.0
        };
        p.circle_filled(egui::pos2(hx, cy), hr, t.text);
        p.circle_stroke(egui::pos2(hx, cy), hr, egui::Stroke::new(1.5, t.well));
        p.rect_filled(cr, kit::R_CTRL, t.well);
        p.text(
            cr.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:+.*} {}", decimals, *value, unit),
            egui::FontId::monospace(kit::T_CAPTION),
            signed(pal.boost, pal.cut, t.dim),
        );
    });

    // Scale strip — min / 0 / max aligned under the track width.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let (_sp, _) = ui.allocate_exact_size(egui::vec2(label_w, 11.0), egui::Sense::hover());
        let (sr, _) = ui.allocate_exact_size(egui::vec2(sw, 11.0), egui::Sense::hover());
        let p = ui.painter();
        let font = egui::FontId::proportional(9.0);
        p.text(
            egui::pos2(sr.left() + pad, sr.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{lo:+.0}"),
            font.clone(),
            t.faint,
        );
        p.text(
            egui::pos2(sr.right() - pad, sr.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{hi:+.0}"),
            font.clone(),
            t.faint,
        );
        let zx = sr.left() + pad + zero_frac * (sr.width() - 2.0 * pad);
        p.text(
            egui::pos2(zx, sr.center().y),
            egui::Align2::CENTER_CENTER,
            "0",
            font,
            t.faint,
        );
    });

    changed
}

/// Trim `text` with a trailing ellipsis so it fits `max_w` at `size`, measured
/// against the live fonts (the thumbnail's "Stacking on …" label).
fn elide(ui: &egui::Ui, text: &str, size: f32, max_w: f32) -> String {
    if kit::text_width(ui, size, text) <= max_w {
        return text.to_string();
    }
    let mut s = text.to_string();
    while !s.is_empty() && kit::text_width(ui, size, &format!("{s}…")) > max_w {
        s.pop();
    }
    format!("{}…", s.trim_end())
}
