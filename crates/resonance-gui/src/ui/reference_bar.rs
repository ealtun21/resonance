//! The compact "Reference" bar under the FR graph, plus the inline target
//! customizer and the Auto-EQ action. Off by default (master toggle) so the
//! graph stays pure EQ until the user opts in — keeping the main UI clean.

use crate::app::GuiApp;
use crate::download::{DlCmd, ModelEntry};
use crate::reference::Channel;
use crate::state::Snapshot;
use crate::ui::kit;
use crate::ui::widgets::dialog_window;
use eframe::egui;
use resonance_autoeq::{BandKind, Smoothing};
use resonance_ipc::{BandState, BandType};

impl GuiApp {
    /// Control strip: master toggle, target + compare pickers, measurement chip,
    /// the two view toggles, Auto-EQ, and the customizer expander. Reflows narrow.
    pub(crate) fn reference_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(kit::SP_XS);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = kit::SP_S;
            let _ = kit::toggle(ui, &mut self.reference.enabled);
            ui.label(egui::RichText::new("Reference").size(kit::T_BODY).strong());

            if !self.reference.enabled {
                ui.label(
                    egui::RichText::new("overlay a target + measurement to EQ by eye")
                        .size(kit::T_CAPTION)
                        .weak(),
                );
                return;
            }

            // ── Target picker ──
            ui.label(egui::RichText::new("target").size(kit::T_CAPTION).weak());
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

            // ── Measurement chip + actions ──
            // The measurement chip IS the Browse trigger — click it to pick a
            // model. Label is ellipsized so it can't overflow the bar.
            ui.label(egui::RichText::new("meas").size(kit::T_CAPTION).weak());
            let has_meas = self.reference.measurement.is_some();
            let mlabel = if has_meas {
                crate::ui::widgets::ellipsize(&self.reference.measurement_name, 22)
            } else {
                "load measurement…".to_string()
            };
            if kit::button(ui, &mlabel, false, true) {
                self.reference.show_browser = true;
            }
            if kit::button(ui, "file…", false, true) {
                self.open_curve_picker(false);
            }
            if has_meas {
                if kit::icon_button(ui, "✕", 22.0) {
                    self.reference.clear_measurement();
                }
                if matches!(&self.reference.measurement_lr, Some((_, Some(_))))
                    && kit::button(ui, self.reference.channel.label(), false, true)
                {
                    self.reference.channel = match self.reference.channel {
                        Channel::Avg => Channel::Left,
                        Channel::Left => Channel::Right,
                        Channel::Right => Channel::Avg,
                    };
                    self.reference.rebuild_measurement();
                }
                // Save the current measurement as a target curve to EQ toward.
                if kit::button(ui, "→ target", false, true) {
                    if let Some(m) = self.reference.measurement.clone() {
                        let name = self.reference.measurement_name.clone();
                        self.reference.save_target(&name, &m);
                    }
                }
            }

            // ── View toggles (only meaningful with a measurement) ──
            if self.reference.measurement.is_some() {
                let _ = kit::toggle(ui, &mut self.reference.show_measurement);
                ui.label(egui::RichText::new("raw meas").size(kit::T_CAPTION).weak());
                let _ = kit::toggle(ui, &mut self.reference.normalized);
                ui.label(egui::RichText::new("normalize").size(kit::T_CAPTION).weak());
            }

            // ── Auto-EQ (fit bands to bring the measurement to the target) ──
            let busy = self.autoeq_busy;
            let can_auto =
                self.reference.measurement.is_some() && self.reference.target.is_some() && !busy;
            let label = if busy { "Auto-EQ…" } else { "Auto-EQ" };
            if kit::button(ui, label, true, can_auto) {
                self.auto_eq(ui.ctx().clone());
            }

            // ── Customizer expander (accent-filled while open) ──
            if kit::button(
                ui,
                "Customize target curve",
                self.reference.adjust_open,
                true,
            ) {
                self.reference.adjust_open = !self.reference.adjust_open;
            }
        });

        if self.reference.enabled && self.reference.adjust_open {
            self.reference_customizer(ui);
        }
    }

    /// The on-the-fly target customizer: tilt + bass / ear-gain / treble shelves
    /// stacked on whichever target is selected. Reset zeroes; Save bakes it into
    /// the curve library.
    fn reference_customizer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(kit::SP_XS);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Shapes the selected target: tilt the overall balance, and lift/cut bass, \
                     ear-gain (~3 kHz) and treble. Stacks on any target — Save stores the result.",
                )
                .size(kit::T_CAPTION)
                .weak(),
            );

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

            // Name + library actions: Save (named), Delete (user targets only),
            // Import a curve from a file, Reset adjustments.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = kit::SP_S;
                if kit::button(ui, "Reset", false, true) {
                    self.reference.adj_tilt = 0.0;
                    self.reference.adj_bass = 0.0;
                    self.reference.adj_ear = 0.0;
                    self.reference.adj_treble = 0.0;
                    changed = true;
                }
                ui.label(egui::RichText::new("name").size(kit::T_CAPTION).weak());
                kit::text_field(
                    ui,
                    150.0,
                    ui.make_persistent_id("ref_target_name"),
                    &mut self.reference.target_name,
                    "custom target name",
                    false,
                );
                let savable = self.reference.target.is_some();
                if kit::button(ui, "Save", false, savable) {
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
                if let Some(path) = self.reference.active_user_target_path() {
                    if kit::button(ui, "Delete", false, true) {
                        self.reference.delete_target(&path);
                    }
                }
                if kit::button(ui, "Import file…", false, true) {
                    self.open_curve_picker(true);
                }
            });

            if changed {
                self.reference.rebuild_target();
            }
        });
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
                        if kit::button(ui, "Refresh", false, true) {
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
                        if kit::button(ui, "All", false, true) {
                            set_all = Some(true);
                        }
                        if kit::button(ui, "None", false, true) {
                            set_all = Some(false);
                        }
                        if kit::button(ui, "Default", false, true) {
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
                                        if kit::button(ui, &lbl, s.enabled, true) {
                                            toggle = Some((s.id.clone(), !s.enabled));
                                        }
                                    }
                                }
                            });
                        });
                    ui.separator();
                    if kit::button(ui, "Close", false, true) {
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
                    if kit::button(ui, "↑ Up", false, true) {
                        browser.parent();
                    }
                    if kit::button(ui, "Home", false, true) {
                        browser.navigate(crate::browser::home_dir());
                    }
                    if kit::button(ui, "Curves", false, true) {
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
