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

        // Disabled: just the master toggle + a one-line hint.
        if !self.reference.enabled {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                let _ = kit::toggle(ui, &mut self.reference.enabled);
                ui.label(egui::RichText::new("Reference").size(kit::T_BODY).strong());
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
        let show_label = ui.available_width() > 600.0;

        // Controls present right now (some need a measurement), in display order.
        let present: Vec<(RefCtl, Section, bool, u8)> = REF_LAYOUT
            .into_iter()
            .filter(|&(c, ..)| ctl_present(c, has_meas, has_stereo))
            .collect();

        // Width fit: start with everything inline, then drop the lowest-priority
        // non-core control until the row fits — accounting for the inter-section
        // hairlines and (only when something has collapsed) the ☰ button.
        let leading = 36.0
            + gap
            + if show_label {
                kit::text_width(ui, kit::T_BODY, "Reference") + gap
            } else {
                0.0
            };
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
            let _ = kit::toggle(ui, &mut self.reference.enabled);
            if show_label {
                ui.label(egui::RichText::new("Reference").size(kit::T_BODY).strong());
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

    /// Inline width a control occupies (used by the collapse fit).
    fn ref_ctl_width(&self, ui: &egui::Ui, c: RefCtl, busy: bool) -> f32 {
        let tgl = |s: &str| 36.0 + kit::SP_XS + kit::text_width(ui, kit::T_CAPTION, s);
        match c {
            RefCtl::TargetDd => 150.0,
            RefCtl::Customize => kit::icon_text_width(ui, "Customize target"),
            RefCtl::Manage => kit::CTRL_H,
            RefCtl::MeasChip => kit::text_width(ui, kit::T_BODY, &self.meas_label()) + 24.0,
            RefCtl::Clear => kit::CTRL_H,
            RefCtl::Channel => {
                kit::text_width(ui, kit::T_BODY, self.reference.channel.label()) + 24.0
            }
            RefCtl::MeasFile => kit::CTRL_H,
            RefCtl::ToTarget => kit::CTRL_H,
            RefCtl::Raw => tgl("Raw"),
            RefCtl::Bounds => tgl("Bounds"),
            RefCtl::Normalize => tgl("Normalize"),
            RefCtl::AutoEq => kit::icon_text_width(ui, if busy { "Auto-EQ…" } else { "Auto-EQ" }),
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

    /// Render one control inline. Secondary actions are icon-only with a hover
    /// tooltip naming them; Auto-EQ/Customize keep their labels (primary).
    fn ref_ctl_inline(&mut self, ui: &mut egui::Ui, c: RefCtl, can_auto: bool, busy: bool) {
        match c {
            RefCtl::TargetDd => {
                let opts = self.reference.target_options();
                let labels: Vec<&str> = opts.iter().map(|(n, _)| n.as_str()).collect();
                let cur = self.reference.target_label();
                if let Some(i) = kit::dropdown(
                    ui,
                    150.0,
                    kit::CTRL_H,
                    ui.make_persistent_id("ref_target_dd"),
                    &cur,
                    &labels,
                ) {
                    self.reference.set_target(opts[i].1.clone());
                }
            }
            RefCtl::Customize => self.ref_customize_button(ui),
            RefCtl::Manage => {
                if kit::icon_btn(
                    ui,
                    Icon::Download,
                    kit::CTRL_H,
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
                if kit::button_tip(ui, &label, false, true, tip) {
                    self.open_browser();
                }
            }
            RefCtl::Clear => {
                if kit::icon_btn(
                    ui,
                    Icon::Close,
                    kit::CTRL_H,
                    "Remove the loaded measurement",
                ) {
                    self.reference.clear_measurement();
                }
            }
            RefCtl::Channel => {
                if kit::button_tip(
                    ui,
                    self.reference.channel.label(),
                    false,
                    true,
                    "Measurement channel: cycle L+R average / Left / Right",
                ) {
                    self.cycle_channel();
                }
            }
            RefCtl::ToTarget => {
                if kit::icon_btn(
                    ui,
                    Icon::Save,
                    kit::CTRL_H,
                    "Save this measurement as a target curve to EQ toward",
                ) {
                    self.meas_to_target();
                }
            }
            RefCtl::MeasFile => {
                if kit::icon_btn(
                    ui,
                    Icon::Folder,
                    kit::CTRL_H,
                    "Load a measurement from a local .txt/.csv file",
                ) {
                    self.open_curve_picker(false);
                }
            }
            RefCtl::Raw => {
                let mut v = self.reference.show_measurement;
                if kit::toggle(ui, &mut v) {
                    self.reference.show_measurement = v;
                }
                ui.label(egui::RichText::new("Raw").size(kit::T_CAPTION).weak())
                    .on_hover_text("Also draw the raw (un-EQ'd) measurement");
            }
            RefCtl::Bounds => {
                let mut v = self.reference.show_bounds;
                if kit::toggle(ui, &mut v) {
                    self.reference.show_bounds = v;
                }
                ui.label(egui::RichText::new("Bounds").size(kit::T_CAPTION).weak())
                    .on_hover_text(
                        "Shade the listener-preference tolerance band around the target \
                         (tight in the mids, wider in bass/treble) — keep the result inside it",
                    );
            }
            RefCtl::Normalize => {
                let mut v = self.reference.normalized;
                if kit::toggle(ui, &mut v) {
                    self.reference.normalized = v;
                }
                ui.label(egui::RichText::new("Normalize").size(kit::T_CAPTION).weak())
                    .on_hover_text("Flatten the target to 0 dB; show the EQ'd result as deviation");
            }
            RefCtl::AutoEq => {
                let label = if busy { "Auto-EQ…" } else { "Auto-EQ" };
                if kit::icon_text_btn(
                    ui,
                    Icon::Wand,
                    label,
                    true,
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
        kit::icon_popup_button(
            ui,
            Icon::Sliders,
            "Customize target",
            "Shape the selected target: tilt + bass / ear-gain / treble shelves (stacks on any target)",
            id,
            320.0,
            |ui| self.reference_customizer_body(ui),
        );
    }

    /// The on-the-fly target customizer body (inside the anchored popup): tilt +
    /// bass / ear-gain / treble shelves stacked on whichever target is selected.
    /// Reset zeroes the adjustments; Save bakes the result into the library.
    fn reference_customizer_body(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "Shapes the selected target: tilt the overall balance, and lift/cut bass, \
                 ear-gain (~3 kHz) and treble. Stacks on any target — Save stores the result.",
            )
            .size(kit::T_CAPTION)
            .weak(),
        );
        ui.add_space(kit::SP_XS);

        let mut changed = false;
        changed |= cust_slider(
            ui,
            "Tilt",
            &mut self.reference.adj_tilt,
            -2.0..=1.0,
            "dB/oct",
            2,
        );
        changed |= cust_slider(
            ui,
            "Bass",
            &mut self.reference.adj_bass,
            -12.0..=18.0,
            "dB",
            1,
        );
        changed |= cust_slider(
            ui,
            "Ear",
            &mut self.reference.adj_ear,
            -12.0..=12.0,
            "dB",
            1,
        );
        changed |= cust_slider(
            ui,
            "Treble",
            &mut self.reference.adj_treble,
            -12.0..=12.0,
            "dB",
            1,
        );

        ui.add_space(kit::SP_XS);
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
                                if ui.selectable_label(false, label).clicked() {
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
                egui::Frame::group(ui.style()).show(ui, |ui| {
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
                                let resp = ui.selectable_label(
                                    i == browser.cursor,
                                    format!("{glyph}  {}", it.name),
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
fn cust_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    unit: &str,
    decimals: usize,
) -> bool {
    let mut changed = false;
    kit::control_row(ui, 54.0, label, |ui| {
        changed = kit::slider(ui, 200.0, value, range);
        kit::value_chip(ui, 70.0, &format!("{:+.*} {}", decimals, *value, unit));
    });
    changed
}
