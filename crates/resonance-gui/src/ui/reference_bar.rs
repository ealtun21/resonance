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
    CaptureResult,
    Raw,
    Bounds,
    Normalize,
    AutoEq,
}

/// `(control, section, core, drop_priority)` in left→right display order. `core`
/// controls never collapse (the target picker + measurement chip — the minimum
/// to be useful); among the rest, the LOWEST `drop_priority` collapses into the
/// ☰ menu first as the bar narrows, so the view toggles go before Auto-EQ/Clear.
const REF_LAYOUT: [(RefCtl, Section, bool, u8); 13] = [
    (RefCtl::TargetDd, Section::Targets, true, 0),
    (RefCtl::Customize, Section::Targets, false, 8),
    (RefCtl::Manage, Section::Targets, false, 7),
    (RefCtl::MeasChip, Section::Measurements, true, 0),
    (RefCtl::Clear, Section::Measurements, false, 9),
    (RefCtl::Channel, Section::Measurements, false, 6),
    (RefCtl::MeasFile, Section::Measurements, false, 5),
    (RefCtl::ToTarget, Section::Measurements, false, 4),
    (RefCtl::CaptureResult, Section::Measurements, false, 0),
    (RefCtl::Raw, Section::Measurements, false, 3),
    (RefCtl::Bounds, Section::Measurements, false, 2),
    (RefCtl::Normalize, Section::Measurements, false, 1),
    (RefCtl::AutoEq, Section::AutoEq, false, 10),
];

const SECTIONS: [Section; 3] = [Section::Targets, Section::Measurements, Section::AutoEq];

/// Max rows rendered in each manage-dialog list before the rest are hidden — a
/// guard so a huge federated catalog can't stall the immediate-mode layout.
const MANAGE_LIST_CAP: usize = 400;

/// Max rows rendered in the browse-dialog model list (with a "+N more" hint).
const BROWSE_LIST_CAP: usize = 300;

/// Actions a single manage-dialog frame can request, collected while `reference`
/// is borrowed and applied once that borrow is released. Source-toggle actions
/// (shared with the browse dialog) live in [`SourceActions`] instead.
#[derive(Default)]
struct ManageActions {
    /// A catalog target curve the user pressed "Add" on.
    fetch_target: Option<TargetEntry>,
    /// A catalog measurement to install as a target (L+R averaged).
    add_meas: Option<ModelEntry>,
    /// A library label the user pressed the remove button on.
    remove_label: Option<String>,
    /// "Reset to defaults" pressed — un-hide every built-in target.
    do_reset: bool,
    /// "Close" pressed.
    close: bool,
}

/// The squig.link source-list actions shared by the browse and manage dialogs.
/// Collected by [`GuiApp::ref_sources_panel`] and dispatched by
/// [`GuiApp::apply_source_actions`].
#[derive(Default)]
struct SourceActions {
    /// "Refresh" pressed — re-fetch the source indexes.
    refresh: bool,
    /// `(source_id, enabled)` for a single toggled source.
    toggle: Option<(String, bool)>,
    /// `Some(on)` for "All"/"None" — enable or disable every source.
    set_all: Option<bool>,
    /// "Default" pressed — restore the built-in source selection.
    set_default: bool,
}

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
        // Same right-anchor daylight rule as the toolbar's `min_flex`: the ☰ is
        // pinned to the right edge, so an inline set that merely *fits* can
        // leave zero gap between the last pill and the ☰ (the widths above are
        // estimates that under-measure the painter-drawn pills by a few px).
        // Whenever the ☰ is present, demand visible daylight on top of the
        // summed widths so the next control drops while they're still apart.
        let min_flex = 2.0 * sep_unit;
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
                + if collapsed_any {
                    overflow_w + min_flex
                } else {
                    0.0
                };
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
                    Self::tb_sep(ui);
                }
                for c in items {
                    self.ref_ctl_inline(ui, c, can_auto, busy);
                }
                drawn = true;
            }

            // ☰ overflow — only when something collapsed. Anchored to the right
            // edge like the toolbar's ⚙/?/☰ cluster; the flexible gap (not a
            // hairline) separates it from the inline pills.
            if !collapsed.is_empty() {
                // Right-anchoring assumes the inline pills left it room; when
                // even the never-collapsing core pills overfill the row, the
                // anchor would draw the ☰ *on top of* them — fall back to the
                // plain inline spot (clipping at the edge like any overfull
                // row) instead.
                if ui.available_width() >= overflow_w + kit::SP_XS + min_flex {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(kit::SP_XS);
                        self.ref_overflow(ui, &collapsed, can_auto, busy);
                    });
                } else {
                    self.ref_overflow(ui, &collapsed, can_auto, busy);
                }
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
            RefCtl::CaptureResult => icon_label("Capture EQ'd"),
            // Icon-only ghost pills all share the same fixed width.
            RefCtl::Manage | RefCtl::Clear | RefCtl::MeasFile | RefCtl::ToTarget => icon_only,
            RefCtl::MeasChip => label_only(&self.meas_label()),
            RefCtl::Channel => label_only(self.reference.channel.label()),
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

    /// Render one control inline as a pill (mockup `.refbar`). Dispatches to a
    /// per-control renderer so each control's pill shape stays a small, named
    /// unit. Secondary actions are icon-only ghost pills with a hover tooltip;
    /// the view flags are check-pills; Auto-EQ is an accent pill.
    fn ref_ctl_inline(&mut self, ui: &mut egui::Ui, c: RefCtl, can_auto: bool, busy: bool) {
        match c {
            RefCtl::TargetDd => self.ref_target_dropdown(ui),
            RefCtl::Customize => self.ref_customize_button(ui),
            RefCtl::Manage => self.ref_manage_pill(ui),
            RefCtl::MeasChip => self.ref_meas_chip(ui),
            RefCtl::Clear => self.ref_clear_pill(ui),
            RefCtl::Channel => self.ref_channel_pill(ui),
            RefCtl::ToTarget => self.ref_to_target_pill(ui),
            RefCtl::CaptureResult => self.ref_capture_button(ui),
            RefCtl::MeasFile => self.ref_meas_file_pill(ui),
            RefCtl::Raw => self.ref_raw_check(ui),
            RefCtl::Bounds => self.ref_bounds_check(ui),
            RefCtl::Normalize => self.ref_normalize_check(ui),
            RefCtl::AutoEq => self.ref_autoeq_pill(ui, can_auto, busy),
        }
    }

    /// The "Target" selector pill: an accent prefix label + the value dropdown,
    /// sized to the pill row height (mockup `.ddl`).
    fn ref_target_dropdown(&mut self, ui: &mut egui::Ui) {
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

    /// "Manage targets" ghost pill — opens the library dialog.
    fn ref_manage_pill(&mut self, ui: &mut egui::Ui) {
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

    /// The measurement chip: shows the loaded name (or a load prompt) and opens
    /// the measurement browser when clicked.
    fn ref_meas_chip(&mut self, ui: &mut egui::Ui) {
        let tip = if self.reference.measurement.is_some() {
            "Measurement to overlay — click to pick another"
        } else {
            "Pick a headphone/IEM measurement from squig.link to overlay"
        };
        let label = self.meas_label();
        if kit::pill_icon(ui, None, &label, false, false, true, tip) {
            self.open_browser();
        }
    }

    /// Clear-measurement ghost pill.
    fn ref_clear_pill(&mut self, ui: &mut egui::Ui) {
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

    /// Channel-cycle pill (L+R average / Left / Right).
    fn ref_channel_pill(&mut self, ui: &mut egui::Ui) {
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

    /// "Save measurement as target" ghost pill.
    fn ref_to_target_pill(&mut self, ui: &mut egui::Ui) {
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

    /// "Load measurement from file" ghost pill.
    fn ref_meas_file_pill(&mut self, ui: &mut egui::Ui) {
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

    /// "Raw" view check-pill — also draw the un-EQ'd measurement.
    fn ref_raw_check(&mut self, ui: &mut egui::Ui) {
        if kit::pill_check(
            ui,
            "Raw",
            self.reference.show_measurement,
            "Also draw the raw (un-EQ'd) measurement",
        ) {
            self.reference.show_measurement = !self.reference.show_measurement;
        }
    }

    /// "Bounds" view check-pill — shade the listener-preference tolerance band.
    fn ref_bounds_check(&mut self, ui: &mut egui::Ui) {
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

    /// "Normalize" view check-pill — flatten the target to 0 dB.
    fn ref_normalize_check(&mut self, ui: &mut egui::Ui) {
        if kit::pill_check(
            ui,
            "Normalize",
            self.reference.normalized,
            "Flatten the target to 0 dB; show the EQ'd result as deviation",
        ) {
            self.reference.normalized = !self.reference.normalized;
        }
    }

    /// Auto-EQ accent pill — enabled only when a measurement and target are both
    /// present and no fit is already running.
    fn ref_autoeq_pill(&mut self, ui: &mut egui::Ui, can_auto: bool, busy: bool) {
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
            RefCtl::CaptureResult => {
                kit::menu_caption(ui, "Capture EQ'd result");
                self.ref_capture_body(ui);
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

    /// "Capture EQ'd result" — a popup with a name field (seeded from the
    /// measurement) and a Save button that bakes the current measurement+EQ into
    /// the target library. Sibling of `meas_to_target`, which saves the raw
    /// (un-EQ'd) measurement.
    fn ref_capture_button(&mut self, ui: &mut egui::Ui) {
        let id = ui.make_persistent_id("ref_capture_pop");
        kit::pill_popup(
            ui,
            Some(Icon::Save),
            "Capture EQ'd",
            "Save the current EQ'd result (measurement + EQ) as a reusable target",
            id,
            300.0,
            |ui| self.ref_capture_body(ui),
        );
    }

    fn ref_capture_body(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Save the current EQ'd result (measurement + EQ) as a target.")
                .size(kit::T_CAPTION)
                .weak(),
        );
        ui.add_space(kit::SP_XS);
        // Seed the name field from the measurement the first time it's empty.
        if self.reference.capture_name.trim().is_empty() {
            self.reference.capture_name = self.reference.eqd_target_default_name();
        }
        let has_meas = self.reference.measurement.is_some();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            kit::text_field(
                ui,
                180.0,
                ui.make_persistent_id("ref_capture_name"),
                &mut self.reference.capture_name,
                "target name…",
                false,
            );
            if kit::button_tip(
                ui,
                "Save",
                true,
                has_meas,
                "Capture the EQ'd result into the target library",
            ) {
                self.capture_result_to_target();
            }
        });
    }

    fn capture_result_to_target(&mut self) {
        let Some((bands, sr)) = self
            .state
            .as_ref()
            .map(|st| (st.bands.clone(), st.sample_rate))
        else {
            self.set_status("no daemon connection");
            return;
        };
        let name = {
            let n = self.reference.capture_name.trim();
            if n.is_empty() {
                self.reference.eqd_target_default_name()
            } else {
                n.to_string()
            }
        };
        if let Some(curve) = self.reference.result_curve(&bands, sr) {
            self.reference.save_target(&name, &curve);
            self.reference.capture_name.clear();
            self.set_status(format!("captured EQ'd target: {name}"));
        } else {
            self.set_status("load a measurement first");
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
        // Actions are collected inside the dialog closure (where `reference` is
        // borrowed) and applied afterwards, once those borrows are released.
        let mut acts = ManageActions::default();
        let mut sources = SourceActions::default();

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
                egui::Panel::top("manage_top").show_inside(ui, |ui| {
                    Self::manage_top_panel(ui, reference, dl_status, dl_busy, &mut sources.refresh);
                });
                // Sources + Close (bottom, fixed) — same federation toggles as the
                // measurement browser; enabling more sites surfaces more targets.
                egui::Panel::bottom("manage_bottom").show_inside(ui, |ui| {
                    Self::ref_sources_panel(ui, catalog, "manage_sources", 64.0, &mut sources);
                    ui.separator();
                    if kit::button_tip(ui, "Close", false, true, "Close this dialog") {
                        acts.close = true;
                    }
                    ui.add_space(2.0);
                });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("manage_list")
                        .auto_shrink([false, false])
                        .show(ui, |ui| match reference.manage_tab {
                            ManageTab::Targets => {
                                Self::manage_targets_tab(ui, reference, catalog, &mut acts);
                            }
                            ManageTab::Measurements => {
                                Self::manage_measurements_tab(ui, reference, catalog, &mut acts);
                            }
                            ManageTab::Yours => {
                                Self::manage_yours_tab(ui, &lib, hidden, &mut acts);
                            }
                        });
                });
            });

        self.apply_manage_actions(acts);
        self.apply_source_actions(&sources);
        if !open {
            self.reference.show_manage = false;
        }
    }

    /// Top panel of the manage dialog: tab buttons plus the per-tab search row.
    /// Sets `refresh` when the Refresh button is pressed.
    fn manage_top_panel(
        ui: &mut egui::Ui,
        reference: &mut resonance_reference::reference::ReferenceState,
        dl_status: &str,
        dl_busy: bool,
        refresh: &mut bool,
    ) {
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
        // The "Your targets" tab is the library itself, so it needs no search.
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
                    *refresh = true;
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
    }

    /// "Target curves" tab: filtered squig.link target list with an Add button
    /// per row. Capped to `MANAGE_LIST_CAP` visible rows.
    fn manage_targets_tab(
        ui: &mut egui::Ui,
        reference: &resonance_reference::reference::ReferenceState,
        catalog: Option<&resonance_reference::download::Catalog>,
        acts: &mut ManageActions,
    ) {
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
            if shown > MANAGE_LIST_CAP {
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
                    acts.fetch_target = Some(t.clone());
                }
                ui.label(format!("{}   ·  {}", t.name, t.source));
            });
        }
        if shown == 0 {
            ui.weak("no target curves — enable more sources below, or Refresh");
        }
    }

    /// "Measurements" tab: filtered squig.link model list; each Add installs the
    /// model (L+R averaged) as a target. Capped to `MANAGE_LIST_CAP` rows.
    fn manage_measurements_tab(
        ui: &mut egui::Ui,
        reference: &resonance_reference::reference::ReferenceState,
        catalog: Option<&resonance_reference::download::Catalog>,
        acts: &mut ManageActions,
    ) {
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
            if shown > MANAGE_LIST_CAP {
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
                    acts.add_meas = Some(m.clone());
                }
                ui.label(format!("{}  ·  {} · {}", m.display, m.source, m.kind));
            });
        }
        if shown == 0 {
            ui.weak("type to search, or enable more sources below");
        }
    }

    /// "Your targets" tab: the current library with a per-row remove button and a
    /// "Reset to defaults" action that un-hides every built-in target.
    fn manage_yours_tab(
        ui: &mut egui::Ui,
        lib: &[resonance_reference::reference::LibEntry],
        hidden: usize,
        acts: &mut ManageActions,
    ) {
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
                acts.do_reset = true;
            }
        });
        ui.add_space(kit::SP_XS);
        if lib.is_empty() {
            ui.weak("no targets — add some from the other tabs");
        }
        for e in lib {
            ui.horizontal(|ui| {
                let tip = if e.builtin {
                    "Hide this built-in / generated target"
                } else {
                    "Delete this user target file"
                };
                if kit::icon_btn(ui, Icon::Close, 22.0, tip) {
                    acts.remove_label = Some(e.label.clone());
                }
                let suffix = if e.builtin { "" } else { "  (added)" };
                ui.label(format!("{}{}", e.label, suffix));
            });
        }
    }

    /// Apply the actions collected while the manage dialog was open.
    fn apply_manage_actions(&mut self, acts: ManageActions) {
        if let Some(t) = acts.fetch_target {
            let _ = self.dl_tx.send(DlCmd::FetchTarget(t));
        }
        if let Some(m) = acts.add_meas {
            let _ = self.dl_tx.send(DlCmd::AddMeasurementTarget(m));
        }
        if let Some(label) = acts.remove_label {
            self.reference.remove_target_label(&label);
        }
        if acts.do_reset {
            self.reference.reset_targets_to_defaults();
        }
        if acts.close {
            self.reference.show_manage = false;
        }
    }

    /// Auto-EQ: fit a parametric bank (peqdb's `AutoEQ`, ported to Rust in
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
        let target: Vec<f32> = f
            .iter()
            .map(|&hz| tgt.interp(f64::from(hz)) as f32)
            .collect();
        let measured: Vec<f32> = f
            .iter()
            .map(|&hz| meas.interp(f64::from(hz)) as f32)
            .collect();
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
                        slope_db_oct: resonance_ipc::default_slope_db_oct(),
                        scope: resonance_ipc::BandScope::Stereo,
                        dynamics: None,
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
        let mut sources = SourceActions::default();

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
                // Top + bottom panels fix the window's chrome so the model list
                // fills the middle without the window auto-growing.
                egui::Panel::top("browse_top").show_inside(ui, |ui| {
                    Self::browse_top_panel(ui, query, dl_status, dl_busy, &mut sources.refresh);
                });
                egui::Panel::bottom("browse_bottom").show_inside(ui, |ui| {
                    Self::ref_sources_panel(ui, catalog, "browse_sources", 80.0, &mut sources);
                    ui.separator();
                    if kit::button_tip(ui, "Close", false, true, "Close this dialog") {
                        close = true;
                    }
                    ui.add_space(2.0);
                });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    Self::browse_models_list(ui, catalog, &q, &mut to_fetch);
                });
            });

        // Apply collected actions once the borrows above are released.
        if let Some(m) = to_fetch {
            let _ = self.dl_tx.send(DlCmd::Fetch(m));
        }
        self.apply_source_actions(&sources);
        if !open || close {
            self.reference.show_browser = false;
        }
    }

    /// Search row (top) of the browse dialog: query field + Refresh + a loading
    /// hint + the download status line. Sets `refresh` when Refresh is pressed.
    fn browse_top_panel(
        ui: &mut egui::Ui,
        query: &mut String,
        dl_status: &str,
        dl_busy: bool,
        refresh: &mut bool,
    ) {
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
                *refresh = true;
            }
            if dl_busy {
                ui.label(egui::RichText::new("loading…").weak());
            }
        });
        if !dl_status.is_empty() {
            ui.label(egui::RichText::new(dl_status).size(kit::T_CAPTION).weak());
        }
        ui.add_space(2.0);
    }

    /// The central model list: every catalog model matching the lowercased query
    /// `q`, capped to `BROWSE_LIST_CAP` rows with a "+N more" hint. Clicking a row
    /// sets `to_fetch`.
    fn browse_models_list(
        ui: &mut egui::Ui,
        catalog: Option<&resonance_reference::download::Catalog>,
        q: &str,
        to_fetch: &mut Option<ModelEntry>,
    ) {
        let matches = |m: &ModelEntry| q.is_empty() || m.display.to_lowercase().contains(q);
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
                    if shown > BROWSE_LIST_CAP {
                        continue;
                    }
                    let label = format!("{}   ·  {} · {}", m.display, m.source, m.kind);
                    if kit::list_row(ui, false, &label).clicked() {
                        *to_fetch = Some(m.clone());
                    }
                }
                let total = cat.models.iter().filter(|m| matches(m)).count();
                if total > BROWSE_LIST_CAP {
                    ui.weak(format!(
                        "+{} more — refine your search",
                        total - BROWSE_LIST_CAP
                    ));
                }
            });
    }

    /// The shared "sources" footer used by both the browse and manage dialogs: an
    /// All / None / Default row plus a wrapping grid of per-source toggle chips.
    /// `salt` and `max_h` differ per dialog; everything else is identical.
    /// Collects toggles into `out` for the caller to apply after borrows release.
    fn ref_sources_panel(
        ui: &mut egui::Ui,
        catalog: Option<&resonance_reference::download::Catalog>,
        salt: &str,
        max_h: f32,
        out: &mut SourceActions,
    ) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("sources").size(kit::T_CAPTION).weak());
            if kit::button_tip(ui, "All", false, true, "Enable every squig.link source") {
                out.set_all = Some(true);
            }
            if kit::button_tip(ui, "None", false, true, "Disable every source") {
                out.set_all = Some(false);
            }
            if kit::button_tip(
                ui,
                "Default",
                false,
                true,
                "Restore the built-in default source selection",
            ) {
                out.set_default = true;
            }
        });
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .max_height(max_h)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(kit::SP_XS, kit::SP_XS);
                    if let Some(cat) = catalog {
                        for s in &cat.sources {
                            let lbl = source_chip_label(&s.name, s.enabled, s.loaded);
                            if kit::button_tip(
                                ui,
                                &lbl,
                                s.enabled,
                                true,
                                "Toggle this squig.link source on/off",
                            ) {
                                out.toggle = Some((s.id.clone(), !s.enabled));
                            }
                        }
                    }
                });
            });
    }

    /// Dispatch the source-toggle actions collected by [`Self::ref_sources_panel`]
    /// to the download worker. Shared by both squig.link dialogs.
    fn apply_source_actions(&mut self, acts: &SourceActions) {
        if acts.refresh {
            let _ = self.dl_tx.send(DlCmd::Refresh);
        }
        if let Some((id, on)) = &acts.toggle {
            let _ = self.dl_tx.send(DlCmd::SetEnabled(id.clone(), *on));
        }
        if let Some(on) = acts.set_all {
            let _ = self.dl_tx.send(DlCmd::SetAll(on));
        }
        if acts.set_default {
            let _ = self.dl_tx.send(DlCmd::SetDefault);
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
                    let is_file = browser.selected().is_some_and(|it| !it.is_dir);
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
        RefCtl::Clear
        | RefCtl::ToTarget
        | RefCtl::CaptureResult
        | RefCtl::Raw
        | RefCtl::Bounds
        | RefCtl::Normalize => has_meas,
        RefCtl::Channel => has_meas && has_stereo,
    }
}

/// The label for a source toggle chip: append an ellipsis while an enabled
/// source is still loading (`enabled && !loaded`) so the user sees it is in
/// flight; otherwise just the plain name.
fn source_chip_label(name: &str, enabled: bool, loaded: bool) -> String {
    if enabled && !loaded {
        format!("{name}…")
    } else {
        name.to_string()
    }
}

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
    let zero_frac = range_fraction(0.0, lo, hi);
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
                let f = f64::from(((pp.x - x0) / tw).clamp(0.0, 1.0));
                let nv = fraction_to_value(f, lo, hi);
                if (nv - *value).abs() > f64::EPSILON {
                    *value = nv;
                    changed = true;
                }
            }
        }

        let frac = range_fraction(*value, lo, hi);
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

/// Where `value` sits within `lo..=hi` as a 0..=1 fraction, clamped to the ends.
/// Positions the customizer slider handle and its "true 0" tick — computed in
/// f64 then narrowed so asymmetric ranges map exactly. Returns 0 for a
/// degenerate `lo == hi` range.
fn range_fraction(value: f64, lo: f64, hi: f64) -> f32 {
    let span = hi - lo;
    if span == 0.0 {
        return 0.0;
    }
    ((value - lo) / span).clamp(0.0, 1.0) as f32
}

/// The inverse of [`range_fraction`]: the value at fraction `f` of `lo..=hi`.
/// Used when a drag picks a new slider value from the pointer position.
fn fraction_to_value(f: f64, lo: f64, hi: f64) -> f64 {
    lo + f * (hi - lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_chip_label_marks_loading() {
        // Enabled but not yet loaded → loading ellipsis.
        assert_eq!(source_chip_label("Crinacle", true, false), "Crinacle…");
        // Enabled and loaded, disabled, or disabled-and-unloaded → plain name.
        assert_eq!(source_chip_label("Crinacle", true, true), "Crinacle");
        assert_eq!(source_chip_label("Crinacle", false, false), "Crinacle");
        assert_eq!(source_chip_label("Crinacle", false, true), "Crinacle");
    }

    #[test]
    fn ctl_present_gates_on_measurement_and_stereo() {
        // Always-present controls don't need a measurement.
        for c in [
            RefCtl::TargetDd,
            RefCtl::Customize,
            RefCtl::Manage,
            RefCtl::MeasChip,
            RefCtl::MeasFile,
            RefCtl::AutoEq,
        ] {
            assert!(ctl_present(c, false, false));
        }
        // Measurement-only controls appear once a measurement is loaded.
        for c in [
            RefCtl::Clear,
            RefCtl::ToTarget,
            RefCtl::CaptureResult,
            RefCtl::Raw,
            RefCtl::Bounds,
            RefCtl::Normalize,
        ] {
            assert!(!ctl_present(c, false, true));
            assert!(ctl_present(c, true, false));
        }
        // Channel cycle additionally needs a stereo measurement.
        assert!(!ctl_present(RefCtl::Channel, true, false));
        assert!(ctl_present(RefCtl::Channel, true, true));
        assert!(!ctl_present(RefCtl::Channel, false, true));
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact assert on clamped range-fraction endpoints
    fn range_fraction_handles_asymmetric_and_degenerate() {
        // Symmetric range: 0 maps to the geometric centre.
        assert!((range_fraction(0.0, -12.0, 12.0) - 0.5).abs() < 1e-6);
        // Asymmetric range (Tilt −2..+1): 0 is two-thirds of the way up.
        assert!((range_fraction(0.0, -2.0, 1.0) - (2.0 / 3.0)).abs() < 1e-6);
        // Out-of-range values clamp to the ends.
        assert_eq!(range_fraction(-5.0, -2.0, 1.0), 0.0);
        assert_eq!(range_fraction(5.0, -2.0, 1.0), 1.0);
        // Degenerate range can't divide by zero.
        assert_eq!(range_fraction(0.0, 3.0, 3.0), 0.0);
    }

    #[test]
    fn fraction_to_value_round_trips_range_fraction() {
        let (lo, hi) = (-12.0, 18.0);
        for &v in &[-12.0, -3.0, 0.0, 7.5, 18.0] {
            let f = f64::from(range_fraction(v, lo, hi));
            assert!((fraction_to_value(f, lo, hi) - v).abs() < 1e-4);
        }
    }
}
