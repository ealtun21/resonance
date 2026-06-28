//! The EQ response curve panel: draggable band nodes, zoom, and the
//! auto-scaling dB axis.

use crate::app::GuiApp;
use crate::curve;
use crate::state::{GAIN_LIMIT, Q_LIMIT};
use crate::ui::kit;
use crate::ui::widgets::{contrast_color, gain_color, lerp_color};
use eframe::egui;
use resonance_ipc::{BandType, Command, DaemonState};
use std::time::Instant;

/// Spectrum envelope time constants: bars snap up, glide down (drawn behind the
/// response curve so silence shows no fill — never a separate black panel).
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.20;

/// A per-channel result curve: (legend label, colour, points).
type ChCurve = (String, egui::Color32, Vec<(f64, f64)>);

impl GuiApp {
    // ── EQ response curve (draggable nodes) ─────────────────────────────────

    /// The FR "hero": one card holding the head bar, the plot, the selected-band
    /// readout, and the reference bar — all stacked with hairline dividers, like
    /// the mockup (the reference bar is part of the graph card, not a separate
    /// strip below it). The caller frames it as a card; this lays out the nested
    /// panels so the plot fills and the readout + reference bar pin to the bottom.
    pub(crate) fn hero(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // No head bar — the plot runs to the card top (FabFilter-style). The
        // gesture legend lives in the readout line below (right-aligned).
        // Reference bar pinned to the very bottom (its own top rule).
        egui::Panel::bottom("hero_refbar")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                let line = kit::tokens(ui).line;
                let (lr, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                // Draw the rule at the allocated rect's *centre*, not its top edge:
                // a 1px stroke centred on the panel's top clip boundary loses its
                // upper half to the clip, and the surviving ~0.5px rounds to
                // visible-or-not as the layout resizes (the flicker). Centring keeps
                // the full stroke inside the clip rect so it's always drawn.
                ui.painter()
                    .hline(lr.x_range(), lr.center().y, egui::Stroke::new(1.0, line));
                // Inset the pills from the card edges (the rule stays full-bleed).
                egui::Frame::default()
                    .inner_margin(egui::Margin {
                        left: kit::CARD_PAD_X as i8,
                        right: kit::CARD_PAD_X as i8,
                        top: 0,
                        bottom: kit::SP_XS as i8,
                    })
                    .show(ui, |ui| self.reference_bar(ui));
            });
        // Readout line above the reference bar (draws its own top rule).
        egui::Panel::bottom("hero_readout")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| self.graph_readout(ui, state));
        // Plot fills the remaining centre.
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.eq_curve(ui, state));
    }

    /// The hero's bottom readout line (mockup `.readout`): the selected band's
    /// parameters in one mono row, with a right-aligned drag/lock hint — so the
    /// node under edit always names itself without opening the bands table.
    pub(crate) fn graph_readout(&self, ui: &mut egui::Ui, state: &DaemonState) {
        let t = kit::tokens(ui);
        let (line_r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        // Centre of the 1px rect, not its top edge — see `hero`: a rule on the
        // panel's top clip boundary half-clips and flickers visible/invisible on
        // resize; centring keeps the whole stroke inside the clip rect.
        ui.painter().hline(
            line_r.x_range(),
            line_r.center().y,
            egui::Stroke::new(1.0, t.line),
        );
        ui.horizontal(|ui| {
            ui.set_min_height(25.0);
            ui.add_space(kit::CARD_PAD_X);
            let font = egui::FontId::monospace(kit::T_VALUE);
            let sep = "  ·  ";
            match state.bands.get(self.selected_band) {
                Some(b) => {
                    let freq = if b.freq >= 1000.0 {
                        format!("{:.2}k Hz", b.freq / 1000.0)
                    } else {
                        format!("{:.0} Hz", b.freq)
                    };
                    let ch = crate::ui::bands::channel_tag(
                        b.channels,
                        &state.channel_layout,
                        state.channels,
                    );
                    let mut job = egui::text::LayoutJob::default();
                    let mut push = |s: &str, col: egui::Color32| {
                        job.append(
                            s,
                            0.0,
                            egui::TextFormat {
                                font_id: font.clone(),
                                color: col,
                                ..Default::default()
                            },
                        );
                    };
                    push(
                        &format!("Band {}", self.selected_band + 1),
                        self.palette.highlight,
                    );
                    push(sep, t.faint);
                    push(b.band_type.abbrev(), t.text);
                    push(sep, t.faint);
                    push(&freq, t.text);
                    push(sep, t.faint);
                    push(
                        &format!("{:+.1} dB", b.gain_db),
                        gain_color(b.gain_db, &self.palette),
                    );
                    push(sep, t.faint);
                    push(&format!("Q {:.2}", b.q), t.text);
                    if state.channels >= 2 {
                        push(sep, t.faint);
                        push(&ch, t.dim);
                    }
                    if !b.enabled {
                        push(sep, t.faint);
                        push("bypassed", t.faint);
                    }
                    let galley = ui.painter().layout_job(job);
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(galley.size().x, 22.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().galley(
                        egui::pos2(r.left(), r.center().y - galley.size().y / 2.0),
                        galley,
                        t.text,
                    );
                    // Right-aligned: the active lock state when a node is locked,
                    // otherwise the gesture legend (the card has no head bar now,
                    // so this is the one place the gestures are spelled out).
                    let (hint, col) = if self.vlock == Some(self.selected_band) {
                        ("vertical-locked · gain only", self.palette.highlight)
                    } else if self.hlock == Some(self.selected_band) {
                        ("gain-locked · freq only", self.palette.highlight)
                    } else {
                        (
                            "drag = move · right-drag = Q · scroll = zoom · double-right-click = lock axis",
                            t.faint,
                        )
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(kit::CARD_PAD_X);
                        let w = kit::text_width(ui, kit::T_CAPTION, hint);
                        let (hr, _) =
                            ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
                        ui.painter().text(
                            egui::pos2(hr.right(), hr.center().y),
                            egui::Align2::RIGHT_CENTER,
                            hint,
                            egui::FontId::proportional(kit::T_CAPTION),
                            col,
                        );
                    });
                }
                None => {
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 22.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        egui::pos2(r.left(), r.center().y),
                        egui::Align2::LEFT_CENTER,
                        "double-click the graph to add a band",
                        egui::FontId::proportional(kit::T_CAPTION),
                        t.faint,
                    );
                }
            }
        });
    }

    pub(crate) fn eq_curve(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Fill the FR panel so dragging its bottom edge resizes the graph.
        let height = ui.available_height().max(50.0);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        let pal = self.palette;

        painter.rect_filled(rect, 4.0, pal.graph_bg);

        // Inset the plotting area so nodes and the curve never sit flush against
        // the panel edge — a few px of breathing room on every side.
        let plot = rect.shrink2(egui::vec2(8.0, 8.0));

        // Response curve + auto-scaled dB axis: the axis grows to fit the
        // loudest point (curve peak or any band's gain) plus 5 dB headroom, so
        // big boosts/cuts stay on-screen with margin instead of clipping.
        let (vlo, vhi) = self.view_log;
        let zoomed = vlo > curve::LOG_MIN + 1e-6 || vhi < curve::LOG_MAX - 1e-6;
        // Draw the curve from an optimistic copy: while dragging, the daemon's
        // echoed `state` only refreshes at the worker's ~30 Hz poll, so the line
        // would visibly lag the node (which uses the immediate `drag_value`).
        // Patch the dragged band's live freq/gain in so the line and node move
        // together at the display's frame rate.
        let mut bands = state.bands.clone();
        if let (Some(i), Some((f, g))) = (self.drag_band, self.drag_value) {
            if let Some(b) = bands.get_mut(i) {
                b.freq = f;
                b.gain_db = g;
            }
        }
        let pts = curve::curve_points_range(&bands, state.sample_rate, 240, vlo, vhi);
        // Per-channel view: when per-channel EQ is in play, each output channel
        // sees only the bands that target it, so the channels can have different
        // responses. Draw one curve per channel (coloured by channel) instead of
        // the single gain-coloured response, so L/R divergence is visible.
        let channels = state.channels;
        let per_channel = channels > 2 || (self.per_channel_eq && channels >= 2);
        // Reference / measurement overlays (target, result, gap…). Empty when the
        // reference system is off or has nothing to show; when present they
        // replace the bare EQ curve (the EQ is folded into the "result" series).
        // Animate the normalise toggle: `na` eases 0↔1 so the target line visibly
        // flattens to the 0-line and the result/measurement stretch into deviation,
        // instead of snapping. Fed into `series` (the geometry) and the draw below.
        let na = ui.ctx().animate_bool_with_time(
            egui::Id::new("ref_norm_morph"),
            self.reference.norm_view(),
            0.35,
        ) as f64;
        let ref_series = if self.reference.active() {
            self.reference
                .series(&bands, state.sample_rate, 240, vlo, vhi, na)
        } else {
            Vec::new()
        };
        let show_ref = !ref_series.is_empty();
        // Per-channel result curves in reference mode: each channel's measurement
        // shaped by *its* EQ, so per-channel divergence shows even with a
        // target/measurement overlay (the single all-bands "Result" is replaced).
        let per_channel_results: Vec<ChCurve> = if show_ref && per_channel {
            use resonance_reference::reference::SeriesRole;
            (0..channels)
                .map(|c| {
                    let cbands: Vec<_> = bands
                        .iter()
                        .filter(|b| b.channels.contains(c))
                        .cloned()
                        .collect();
                    let pts = self
                        .reference
                        .series(&cbands, state.sample_rate, 240, vlo, vhi, na)
                        .into_iter()
                        .find(|s| s.role == SeriesRole::Result)
                        .map(|s| s.pts)
                        .unwrap_or_default();
                    let label = state
                        .channel_layout
                        .get(c)
                        .map(String::as_str)
                        .unwrap_or("?");
                    (format!("Result {label}"), channel_color(c), pts)
                })
                .collect()
        } else {
            Vec::new()
        };
        // Unified legend entries `(label, colour, dashed)` — each gets an eye
        // toggle. In the normalised view the target IS the flat 0-line, so it's
        // not keyed separately.
        let legend_entries: Vec<(String, egui::Color32, bool)> = if show_ref {
            let mut v: Vec<(String, egui::Color32, bool)> = Vec::new();
            if per_channel {
                for (key, col, _) in &per_channel_results {
                    v.push((key.clone(), *col, false));
                }
            } else {
                v.push(("Result".into(), pal.highlight, false));
            }
            if !self.reference.norm_view() && self.reference.target.is_some() {
                v.push(("Target".into(), pal.accent, true));
            }
            if self.reference.show_bounds && self.reference.target.is_some() {
                v.push(("Bounds".into(), pal.accent.gamma_multiply(0.5), false));
            }
            if self.reference.show_measurement {
                v.push(("Measured".into(), pal.neutral.gamma_multiply(0.7), false));
            }
            v
        } else if per_channel {
            (0..channels)
                .map(|c| {
                    let name = state
                        .channel_layout
                        .get(c)
                        .map(String::as_str)
                        .unwrap_or("?")
                        .to_string();
                    (name, channel_color(c), false)
                })
                .collect()
        } else {
            Vec::new()
        };
        let legend_rect = legend_box_rect(plot, &painter, &legend_entries);
        // Loudest point the axis must show (any band gain, curve peak, or overlay
        // extent) + 5 dB headroom. Includes the dragged band, so the axis expands
        // live as you drag a node up and contracts as you bring it down.
        let ref_peak = ref_series
            .iter()
            .flat_map(|s| s.pts.iter())
            .map(|&(_, y)| y.abs())
            .fold(0.0_f64, f64::max);
        let peak = pts
            .iter()
            .map(|&(_, g)| g.abs())
            .chain(bands.iter().map(|b| b.gain_db.abs()))
            .fold(0.0_f64, f64::max)
            .max(ref_peak);
        // The preference band widens past the target at the extremes — grow the
        // axis to keep the whole band on-screen when it's shown.
        let peak = if self.reference.show_bounds {
            ref_series
                .iter()
                .find(|s| s.role == resonance_reference::reference::SeriesRole::Target)
                .map(|t| {
                    t.pts.iter().fold(peak, |acc, &(lf, y)| {
                        let (below, above) =
                            resonance_reference::reference::preference_bounds(10f64.powf(lf));
                        acc.max(y.abs() + below.max(above))
                    })
                })
                .unwrap_or(peak)
        } else {
            peak
        };
        let needed = peak + 5.0;
        // Pick the ± dB stop with HYSTERESIS so it doesn't chatter (jiggle) when
        // `needed` sits right on a stop boundary: grow once it exceeds 98% of the
        // current stop, shrink only once it drops below 65%. The wide deadband
        // also breaks the gain↔axis feedback — dragging the node up to ~mid-graph
        // settles on a stop instead of running away (only slamming the node to
        // the very top edge takes it to the max stop, which is what you'd want).
        if needed > self.db_target * 0.98 || needed < self.db_target * 0.65 {
            let (t, s) = curve::display_range(needed);
            self.db_target = t;
            self.db_step = s;
        }
        let target_db = self.db_target;
        let db_step = self.db_step;
        // Ease the axis toward the chosen stop so the curve + markers glide
        // instead of snapping. Same easing whether or not a drag is active, so
        // expand and contract feel identical.
        self.db_axis += (target_db - self.db_axis) * 0.20;
        if (self.db_axis - target_db).abs() < 0.05 {
            self.db_axis = target_db;
        }
        let db = self.db_axis;
        let axis_animating = (self.db_axis - target_db).abs() > 1e-3;
        let x_of =
            |logf: f64| -> f32 { plot.left() + ((logf - vlo) / (vhi - vlo)) as f32 * plot.width() };
        let y_of = |gain: f64| -> f32 {
            plot.top() + (1.0 - ((gain + db) / (2.0 * db)) as f32) * plot.height()
        };
        let logf_of =
            |x: f32| -> f64 { vlo + ((x - plot.left()) / plot.width()) as f64 * (vhi - vlo) };
        let db_of =
            |y: f32| -> f64 { ((1.0 - (y - plot.top()) / plot.height()) as f64) * 2.0 * db - db };

        // Frequency-region background bands (sub/bass/lo-mid/hi-mid/treble/air).
        // Alternating faint fills make each tonal region lightly noticeable
        // without competing with the curve; a dim label sits along the top.
        for (i, (lo, hi, label)) in curve::freq_bands().into_iter().enumerate() {
            let xl = x_of(lo.log10()).max(plot.left());
            let xr = x_of(hi.log10()).min(plot.right());
            if xr <= xl {
                continue;
            }
            let band =
                egui::Rect::from_min_max(egui::pos2(xl, plot.top()), egui::pos2(xr, plot.bottom()));
            let alpha = if i % 2 == 0 { 10 } else { 22 };
            let [r, g, b, _] = pal.neutral.to_array();
            painter.rect_filled(
                band,
                0.0,
                egui::Color32::from_rgba_unmultiplied(r, g, b, alpha),
            );
            // Only label a region wide enough to fit the text, so the labels don't
            // collide ("presmid treble air") when the graph is narrow.
            if xr - xl > label.len() as f32 * 5.2 + 6.0 {
                painter.text(
                    egui::pos2((xl + xr) * 0.5, plot.top() + 1.0),
                    egui::Align2::CENTER_TOP,
                    label,
                    egui::FontId::monospace(8.0),
                    egui::Color32::from_rgba_unmultiplied(r, g, b, 150),
                );
            }
        }

        // Horizontal dB grid lines at multiples of the step within ±range.
        let label_col = pal.neutral;
        let grid = egui::Stroke::new(1.0, pal.grid.gamma_multiply(0.6));
        let n_lines = (db / db_step) as i32;
        for k in -n_lines..=n_lines {
            let g = k as f64 * db_step;
            let y = y_of(g);
            let stroke = if g == 0.0 {
                // Emphasised 0 dB reference line.
                egui::Stroke::new(1.6, pal.neutral)
            } else {
                grid
            };
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                stroke,
            );
            painter.text(
                egui::pos2(plot.left() + 2.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{g:+.0}"),
                egui::FontId::monospace(9.0),
                label_col,
            );
        }
        // Vertical frequency grid + labels. Always draw the gridline, but skip a
        // label when it would crowd the previous one (keeps "80 100 150 200"
        // from merging on a narrow graph).
        let mut last_label_x = f32::NEG_INFINITY;
        for (logf, label) in curve::x_axis_ticks_range(vlo, vhi) {
            let x = x_of(logf);
            painter.line_segment(
                [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                grid,
            );
            if x - last_label_x >= 30.0 {
                last_label_x = x;
                painter.text(
                    egui::pos2(x, plot.bottom() - 2.0),
                    egui::Align2::CENTER_BOTTOM,
                    label,
                    egui::FontId::monospace(9.0),
                    label_col,
                );
            }
        }

        // Live spectrum as a translucent fill BEHIND the response curve (the
        // FabFilter/LSP idiom): ambient context on its own magnitude axis off the
        // bottom edge, so it never competes with the curve — and silence simply
        // shows no fill instead of a separate black panel.
        {
            let bins = &state.spectrum;
            let n = bins.len();
            if self.spectrum_display.len() != n {
                self.spectrum_display = vec![0.0; n];
            }
            let dt = self.last_anim.elapsed().as_secs_f32().min(0.1);
            self.last_anim = Instant::now();
            for (disp, &raw) in self.spectrum_display.iter_mut().zip(bins.iter()) {
                let target = raw.clamp(0.0, 1.0);
                let tau = if target > *disp {
                    SPECTRUM_ATTACK_TAU
                } else {
                    SPECTRUM_DECAY_TAU
                };
                *disp += (target - *disp) * (1.0 - (-dt / tau).exp());
            }
            let n = self.spectrum_display.len();
            if n > 0 {
                let range = curve::LOG_MAX - curve::LOG_MIN;
                let base = plot.bottom();
                // Bins are log-spaced over 20 Hz–20 kHz; map each bin's edges
                // through x_of so the fill aligns with the log axis (and clips
                // correctly when the view is zoomed). Per bin: a filled column
                // (brighter toward the peak) so it always reads as a spectrum
                // analyzer. A small minimum body + floor alpha keep a faint
                // analyzer band visible even in silence, so the spectrum never
                // looks "gone" — it just lights up with audio.
                const MIN_BODY: f32 = 0.016;
                // Small inter-bar gap so the analyzer reads as discrete columns
                // rather than one opaque slab — keeps the response curve the focus.
                const GAP: f32 = 1.0;
                for (i, &v) in self.spectrum_display.iter().enumerate() {
                    let v = v.clamp(0.0, 1.0);
                    let lo = curve::LOG_MIN + i as f64 / n as f64 * range;
                    let hi = curve::LOG_MIN + (i + 1) as f64 / n as f64 * range;
                    let x0 = x_of(lo).max(plot.left()) + GAP;
                    let x1 = x_of(hi).min(plot.right()) - GAP;
                    if x1 <= x0 {
                        continue;
                    }
                    let body = (v * 0.90 + MIN_BODY).min(1.0);
                    let top_y = base - body * plot.height();
                    // Gentle hue shift toward the highlight + low alpha ceiling so
                    // the fill stays ambient context behind the curve.
                    let col = lerp_color(pal.accent, pal.highlight, v * 0.5);
                    let [r, g, b, _] = col.to_array();
                    let a = (16.0 + 66.0 * v).min(96.0) as u8;
                    painter.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(x0, top_y), egui::pos2(x1, base)),
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(r, g, b, a),
                    );
                }
            }
        }

        // Response curve. With reference overlays active the EQ response is folded
        // into the "result" series, so we draw the overlays instead of the bare
        // curve; otherwise draw the colour-coded EQ response (boost green / cut
        // red, neutral near 0 dB) as usual.
        let hidden = &self.hidden_curves;
        if show_ref {
            use resonance_reference::reference::SeriesRole;
            let role_key = |r: SeriesRole| match r {
                SeriesRole::Result => "Result",
                SeriesRole::Target => "Target",
                SeriesRole::Measurement => "Measured",
            };
            // Draw the overlay, skipping eye-hidden roles. In per-channel mode the
            // single all-bands Result is dropped (replaced by per-channel ones).
            let filtered: Vec<_> = ref_series
                .iter()
                .filter(|s| {
                    !(hidden.contains(role_key(s.role))
                        || (per_channel && s.role == SeriesRole::Result))
                })
                .map(|s| resonance_reference::reference::RefSeries {
                    role: s.role,
                    pts: s.pts.clone(),
                })
                .collect();
            let show_bounds = self.reference.show_bounds && !hidden.contains("Bounds");
            draw_reference(
                &painter,
                &filtered,
                na as f32,
                show_bounds,
                &x_of,
                &y_of,
                &pal,
            );
            // Per-channel result curves (channel-coloured), each eye-toggleable.
            for (key, col, cpts) in &per_channel_results {
                if hidden.contains(key) {
                    continue;
                }
                for w in cpts.windows(2) {
                    painter.line_segment(
                        [
                            egui::pos2(x_of(w[0].0), y_of(w[0].1)),
                            egui::pos2(x_of(w[1].0), y_of(w[1].1)),
                        ],
                        egui::Stroke::new(2.0, *col),
                    );
                }
            }
        } else if per_channel {
            // One response per channel, from only the bands targeting it.
            for c in 0..channels {
                let label = state
                    .channel_layout
                    .get(c)
                    .map(String::as_str)
                    .unwrap_or("?");
                if hidden.contains(label) {
                    continue;
                }
                let cbands: Vec<_> = bands
                    .iter()
                    .filter(|b| b.channels.contains(c))
                    .cloned()
                    .collect();
                let cpts = curve::curve_points_range(&cbands, state.sample_rate, 240, vlo, vhi);
                let col = channel_color(c);
                for w in cpts.windows(2) {
                    let a = egui::pos2(x_of(w[0].0), y_of(w[0].1));
                    let b = egui::pos2(x_of(w[1].0), y_of(w[1].1));
                    painter.line_segment([a, b], egui::Stroke::new(2.0, col));
                }
            }
        } else {
            for w in pts.windows(2) {
                let (lf0, g0) = w[0];
                let (lf1, g1) = w[1];
                let a = egui::pos2(x_of(lf0), y_of(g0));
                let b = egui::pos2(x_of(lf1), y_of(g1));
                let color = gain_color((g0 + g1) * 0.5, &pal);
                painter.line_segment([a, b], egui::Stroke::new(2.0, color));
            }
        }

        use egui::PointerButton::{Primary, Secondary};

        // The legend (with its eye toggles) sits inside the graph; a click there
        // must toggle a curve, not grab/create a node. Skip node interactions
        // whose pointer is over the legend box.
        let over_legend = response
            .interact_pointer_pos()
            .map(|p| legend_rect.contains(p))
            .unwrap_or(false);

        // ── Zoom ────────────────────────────────────────────────────────────
        // Scroll wheel over the graph zooms the x-axis around the pointer;
        // Shift+left-drag box-selects a frequency range to zoom into. The
        // span is the daemon-wide LOG_MIN..LOG_MAX; we never zoom out past it.
        let shift = ui.input(|i| i.modifiers.shift);
        let full = (curve::LOG_MIN, curve::LOG_MAX);
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(p) = response.hover_pos() {
                    let center = logf_of(p.x).clamp(vlo, vhi);
                    // Positive scroll = zoom in; shrink the span toward center.
                    let factor = (-scroll as f64 * 0.0015).exp();
                    let new_span =
                        ((vhi - vlo) * factor).clamp(0.15, curve::LOG_MAX - curve::LOG_MIN);
                    let t = (center - vlo) / (vhi - vlo);
                    let mut lo = center - t * new_span;
                    let mut hi = lo + new_span;
                    // Slide back inside the full span instead of clipping.
                    if lo < full.0 {
                        hi += full.0 - lo;
                        lo = full.0;
                    }
                    if hi > full.1 {
                        lo -= hi - full.1;
                        hi = full.1;
                    }
                    self.view_log = (lo.max(full.0), hi.min(full.1));
                }
            }
        }
        // Shift+left-drag: select an x-range, zoom to it on release.
        if shift && response.drag_started_by(Primary) {
            if let Some(p) = response.interact_pointer_pos() {
                self.zoom_sel = Some(logf_of(p.x));
            }
        }
        if let Some(start) = self.zoom_sel {
            if let Some(p) = response.interact_pointer_pos() {
                let (a, b) = (x_of(start).min(p.x), x_of(start).max(p.x));
                let band = egui::Rect::from_min_max(
                    egui::pos2(a.max(plot.left()), plot.top()),
                    egui::pos2(b.min(plot.right()), plot.bottom()),
                );
                let [r, g, bl, _] = pal.highlight.to_array();
                painter.rect_filled(
                    band,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(r, g, bl, 40),
                );
                painter.rect_stroke(
                    band,
                    0.0,
                    egui::Stroke::new(1.0, pal.highlight),
                    egui::StrokeKind::Inside,
                );
            }
            if response.drag_stopped() {
                if let Some(p) = response.interact_pointer_pos() {
                    let (lo, hi) = (start.min(logf_of(p.x)), start.max(logf_of(p.x)));
                    // Ignore a stray click; require a meaningful selection width.
                    if hi - lo > 0.05 {
                        self.view_log = (lo.max(full.0), hi.min(full.1));
                    }
                }
                self.zoom_sel = None;
            }
        }
        // "Reset zoom" affordance + current range readout (only when zoomed).
        if zoomed && self.zoom_sel.is_none() {
            let lo_hz = 10f64.powf(vlo).round();
            let hi_hz = 10f64.powf(vhi).round();
            let txt = format!("{lo_hz:.0}–{hi_hz:.0} Hz · reset ⟲");
            let btn = painter.text(
                egui::pos2(plot.right() - 4.0, plot.top() + 2.0),
                egui::Align2::RIGHT_TOP,
                txt,
                egui::FontId::monospace(10.0),
                pal.highlight,
            );
            let hit = btn.expand(3.0);
            if response.clicked() {
                if let Some(p) = response.interact_pointer_pos() {
                    if hit.contains(p) {
                        self.view_log = full;
                    }
                }
            }
        }

        // Double-right-click a node → toggle vertical-lock (gain-only) movement.
        // Shift+double-right-click → toggle gain-lock (freq+Q only, gain pinned).
        // The two locks are mutually exclusive.
        if response.double_clicked_by(Secondary) && !over_legend {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(i) = nearest_band(state, p, &x_of, &y_of) {
                    if ui.input(|inp| inp.modifiers.shift) {
                        self.hlock = if self.hlock == Some(i) { None } else { Some(i) };
                        self.vlock = None;
                    } else {
                        self.vlock = if self.vlock == Some(i) { None } else { Some(i) };
                        self.hlock = None;
                    }
                    self.selected_band = i;
                }
            }
        }

        // Drag handling: left button moves a node (freq+gain), right button
        // tunes its Q (drag up = narrower). A vertical-locked node moves on the
        // gain axis only. Pick the nearest node on press.
        let started_primary = response.drag_started_by(Primary);
        let started_secondary = response.drag_started_by(Secondary);
        if (started_primary || started_secondary) && self.zoom_sel.is_none() && !over_legend {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(i) = nearest_band(state, p, &x_of, &y_of) {
                    self.drag_band = Some(i);
                    self.selected_band = i;
                    // Right button always tunes Q — the vertical lock pins only
                    // frequency, never Q.
                    self.drag_q = started_secondary;
                }
            }
        }
        if let Some(i) = self.drag_band {
            let locked = self.vlock == Some(i);
            let gain_locked = self.hlock == Some(i);
            if self.drag_q && response.dragged_by(Secondary) {
                let dy = response.drag_delta().y as f64;
                if dy != 0.0 {
                    if let Some(b) = state.bands.get(i) {
                        // Exponential so Q scales smoothly across its range.
                        let q = (b.q * (-dy * 0.015).exp()).clamp(0.1, Q_LIMIT);
                        self.queue_edit(Command::SetBand {
                            index: i,
                            freq: b.freq,
                            gain_db: b.gain_db,
                            q,
                        });
                    }
                }
            } else if !self.drag_q && response.dragged_by(Primary) {
                if let Some(p) = response.interact_pointer_pos() {
                    if let Some(b) = state.bands.get(i) {
                        // vlock: keep freq, move gain only.
                        // hlock: keep gain, move freq only.
                        let freq = if locked {
                            b.freq
                        } else {
                            10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0)
                        };
                        let gain = if gain_locked {
                            b.gain_db
                        } else {
                            // Map the cursor through the current axis. Because the
                            // node is rendered with the same `db` (y_of ∘ db_of =
                            // identity), it stays exactly under the cursor; the
                            // axis growth is decoupled (see the peak calc above),
                            // so there's no feedback wobble.
                            db_of(p.y).clamp(-GAIN_LIMIT, GAIN_LIMIT)
                        };
                        // Remember the cursor-derived value so the node renders
                        // there immediately (not at the IPC-lagged echo).
                        self.drag_value = Some((freq, gain));
                        self.queue_edit(Command::SetBand {
                            index: i,
                            freq,
                            gain_db: gain,
                            q: b.q,
                        });
                    }
                }
            }
        }
        if response.drag_stopped_by(Primary) || response.drag_stopped_by(Secondary) {
            self.drag_band = None;
            self.drag_q = false;
            self.drag_value = None;
        }

        // Drive continuous frames (not just on input events) while dragging a
        // node or while the dB axis is still easing, so motion is smooth at the
        // display's refresh rate rather than the OS pointer-event cadence.
        if self.drag_band.is_some() || axis_animating {
            ui.ctx().request_repaint();
        }
        // Double-left-click empty area → add a peaking band there.
        if response.double_clicked_by(Primary) && !over_legend {
            if let Some(p) = response.interact_pointer_pos() {
                let freq = 10f64.powf(logf_of(p.x)).clamp(20.0, 20000.0);
                let gain = db_of(p.y).clamp(-GAIN_LIMIT, GAIN_LIMIT);
                self.queue_edit(Command::AddBand {
                    band_type: BandType::Peaking,
                    freq,
                    gain_db: gain,
                    q: 1.4,
                });
            }
        }

        // Band node markers.
        for (i, b) in state.bands.iter().enumerate() {
            if !b.enabled {
                continue;
            }
            // While this band is being dragged, render it at the cursor-derived
            // value (not the IPC-lagged echo) so the node tracks the mouse.
            let (bf, bg) = match self.drag_value {
                Some(v) if self.drag_band == Some(i) => v,
                _ => (b.freq, b.gain_db),
            };
            // Clamp the node inside the plot so it can never draw off-screen
            // even if the response momentarily exceeds the axis.
            let center = egui::pos2(
                x_of(curve::clampf_log(bf)).clamp(plot.left(), plot.right()),
                y_of(bg).clamp(plot.top(), plot.bottom()),
            );
            let selected = i == self.selected_band;
            let locked = self.vlock == Some(i);
            let gain_locked = self.hlock == Some(i);
            // High-contrast guide derived from the graph background (not the
            // palette accent/highlight, which on some themes — e.g. matugen —
            // matches the curve and nodes). Dashed + thick so it stands out
            // against the grid and the response curve.
            let guide = contrast_color(pal.graph_bg);
            let stroke = egui::Stroke::new(2.0, guide);
            // vlock: vertical guide with end caps ("moves up/down only").
            if locked {
                let x = center.x;
                painter.add(egui::Shape::dashed_line(
                    &[
                        egui::pos2(x, plot.top() + 2.0),
                        egui::pos2(x, plot.bottom() - 2.0),
                    ],
                    stroke,
                    6.0,
                    4.0,
                ));
                for cap_y in [plot.top() + 2.0, plot.bottom() - 2.0] {
                    painter.line_segment(
                        [egui::pos2(x - 5.0, cap_y), egui::pos2(x + 5.0, cap_y)],
                        stroke,
                    );
                }
            }
            // hlock: horizontal guide with end caps ("moves left/right only").
            if gain_locked {
                let y = center.y;
                painter.add(egui::Shape::dashed_line(
                    &[
                        egui::pos2(plot.left() + 2.0, y),
                        egui::pos2(plot.right() - 2.0, y),
                    ],
                    stroke,
                    6.0,
                    4.0,
                ));
                for cap_x in [plot.left() + 2.0, plot.right() - 2.0] {
                    painter.line_segment(
                        [egui::pos2(cap_x, y - 5.0), egui::pos2(cap_x, y + 5.0)],
                        stroke,
                    );
                }
            }
            // Selected node pops; the rest recede hard toward the background so
            // the active band is unmistakable on every theme. The selected node
            // also gets a high-contrast ring (white on dark, black on light).
            let color = if selected {
                pal.highlight
            } else {
                pal.neutral.gamma_multiply(0.45)
            };
            let r = if selected || locked || gain_locked {
                7.0
            } else {
                4.0
            };
            painter.circle_filled(center, r, color);
            let ring = if selected {
                egui::Stroke::new(2.0, contrast_color(pal.graph_bg))
            } else {
                egui::Stroke::new(1.0, pal.graph_bg)
            };
            painter.circle_stroke(center, r, ring);
        }

        // As the view normalises, the 0-line becomes the target — fade the tag in
        // with the morph (`na`), tracking the dashed target line that's flattening
        // onto it.
        if show_ref && self.reference.target.is_some() && na > 0.02 {
            painter.text(
                egui::pos2(plot.right() - 4.0, y_of(0.0) - 3.0),
                egui::Align2::RIGHT_BOTTOM,
                "target",
                egui::FontId::monospace(9.0),
                pal.accent.gamma_multiply(na as f32),
            );
        }

        // Interactive legend (bottom-right): an eye toggle per series to
        // show/hide it — making it easy to isolate one channel/curve while
        // editing. Empty in the plain single-curve view.
        legend_with_eyes(
            &mut self.hidden_curves,
            ui,
            &painter,
            legend_rect,
            &pal,
            &legend_entries,
        );
    }
}

/// Distinct per-channel curve colour (FL blue, FR orange, …) for the
/// per-channel FR view, so left/right (and beyond) read apart at a glance.
/// Shared with the bands table's per-band channel chip so the colours agree.
pub(crate) fn channel_color(c: usize) -> egui::Color32 {
    const COLORS: [egui::Color32; 8] = [
        egui::Color32::from_rgb(90, 175, 255),  // FL  blue
        egui::Color32::from_rgb(255, 150, 90),  // FR  orange
        egui::Color32::from_rgb(120, 220, 120), // FC  green
        egui::Color32::from_rgb(200, 130, 255), // LFE purple
        egui::Color32::from_rgb(255, 215, 90),  // RL  yellow
        egui::Color32::from_rgb(120, 230, 230), // RR  teal
        egui::Color32::from_rgb(255, 120, 170), // pink
        egui::Color32::from_rgb(180, 180, 180), // grey
    ];
    COLORS[c % COLORS.len()]
}

/// Draw the reference/measurement overlay series. Faint context (raw
/// measurement, compare) first, the dashed target next, the bold "result" last
/// so it sits on top.
fn draw_reference(
    painter: &egui::Painter,
    series: &[resonance_reference::reference::RefSeries],
    na: f32,
    show_bounds: bool,
    x_of: &dyn Fn(f64) -> f32,
    y_of: &dyn Fn(f64) -> f32,
    pal: &crate::theme::Palette,
) {
    use resonance_reference::reference::SeriesRole;

    // Preference tolerance band first, behind every line: a translucent ribbon at
    // target ± preference_halfwidth(f), so it follows the target (and flattens to
    // a horizontal band around 0 as the view normalises). Drawn as per-segment
    // filled quads (each a convex trapezoid) plus faint edge lines.
    if show_bounds {
        if let Some(t) = series.iter().find(|s| s.role == SeriesRole::Target) {
            let [r, g, b, _] = pal.accent.to_array();
            let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, 26);
            let edge = egui::Stroke::new(1.0, pal.accent.gamma_multiply(0.45));
            let mut up: Vec<egui::Pos2> = Vec::with_capacity(t.pts.len());
            let mut lo: Vec<egui::Pos2> = Vec::with_capacity(t.pts.len());
            for &(lf, y) in &t.pts {
                let (below, above) =
                    resonance_reference::reference::preference_bounds(10f64.powf(lf));
                up.push(egui::pos2(x_of(lf), y_of(y + above)));
                lo.push(egui::pos2(x_of(lf), y_of(y - below)));
            }
            for i in 0..up.len().saturating_sub(1) {
                painter.add(egui::Shape::convex_polygon(
                    vec![up[i], up[i + 1], lo[i + 1], lo[i]],
                    fill,
                    egui::Stroke::NONE,
                ));
            }
            painter.add(egui::Shape::line(up, edge));
            painter.add(egui::Shape::line(lo, edge));
        }
    }
    let line = |pts: &[(f64, f64)], stroke: egui::Stroke| {
        for w in pts.windows(2) {
            painter.line_segment(
                [
                    egui::pos2(x_of(w[0].0), y_of(w[0].1)),
                    egui::pos2(x_of(w[1].0), y_of(w[1].1)),
                ],
                stroke,
            );
        }
    };
    let dashed = |pts: &[(f64, f64)], stroke: egui::Stroke, dash: f32, gap: f32| {
        let path: Vec<egui::Pos2> = pts
            .iter()
            .map(|&(l, y)| egui::pos2(x_of(l), y_of(y)))
            .collect();
        painter.add(egui::Shape::dashed_line(&path, stroke, dash, gap));
    };
    for s in series {
        match s.role {
            // The raw (un-EQ'd) measurement — in the normalised view this is the
            // error you're correcting; faint so the result stands out.
            SeriesRole::Measurement => line(
                &s.pts,
                egui::Stroke::new(1.0, pal.neutral.gamma_multiply(0.5)),
            ),
            // The dashed target line. Its points already flatten toward the 0-line
            // as `na`→1; fade it out over the last quarter of the morph so it hands
            // off cleanly to the grid's 0-line (tagged "target").
            SeriesRole::Target => {
                let a = ((1.0 - na) / 0.25).clamp(0.0, 1.0);
                if a > 0.01 {
                    dashed(
                        &s.pts,
                        egui::Stroke::new(1.5, pal.accent.gamma_multiply(a)),
                        6.0,
                        4.0,
                    );
                }
            }
            // Result: solid highlight in the absolute view, easing toward the
            // boost/cut gain colouring as the view normalises into deviation.
            SeriesRole::Result => {
                for w in s.pts.windows(2) {
                    let (l0, y0) = w[0];
                    let (l1, y1) = w[1];
                    let color = lerp_color(pal.highlight, gain_color((y0 + y1) * 0.5, pal), na);
                    painter.line_segment(
                        [
                            egui::pos2(x_of(l0), y_of(y0)),
                            egui::pos2(x_of(l1), y_of(y1)),
                        ],
                        egui::Stroke::new(2.5, color),
                    );
                }
            }
        }
    }
}

/// Legend geometry constants: padding, row height, eye glyph, line swatch, gaps.
const LG: (f32, f32, f32, f32, f32) = (6.0, 15.0, 12.0, 16.0, 6.0);

/// The legend box rectangle (bottom-right of the plot), so graph interactions
/// can avoid stealing clicks meant for the legend's eye toggles.
fn legend_box_rect(
    plot: egui::Rect,
    painter: &egui::Painter,
    entries: &[(String, egui::Color32, bool)],
) -> egui::Rect {
    if entries.is_empty() {
        return egui::Rect::NOTHING;
    }
    let (pad, row_h, eye_w, sw, gap) = LG;
    let font = egui::FontId::monospace(9.5);
    let max_w = entries
        .iter()
        .map(|(l, _, _)| {
            painter
                .layout_no_wrap(l.clone(), font.clone(), egui::Color32::WHITE)
                .rect
                .width()
        })
        .fold(0.0_f32, f32::max);
    let box_w = pad + eye_w + gap + sw + gap + max_w + pad;
    let box_h = pad * 2.0 + entries.len() as f32 * row_h;
    let right = plot.right() - 4.0;
    let bottom = plot.bottom() - 16.0;
    egui::Rect::from_min_max(
        egui::pos2(right - box_w, bottom - box_h),
        egui::pos2(right, bottom),
    )
}

/// Draw the legend with a clickable eye toggle per row: clicking a row hides or
/// shows that curve (tracked in `hidden`, keyed by label). Hidden rows render
/// dimmed with a closed-eye glyph. `entries` are `(label, colour, dashed)`.
#[allow(clippy::too_many_arguments)]
fn legend_with_eyes(
    hidden: &mut std::collections::HashSet<String>,
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    pal: &crate::theme::Palette,
    entries: &[(String, egui::Color32, bool)],
) {
    if entries.is_empty() {
        return;
    }
    let (pad, row_h, eye_w, sw, gap) = LG;
    let font = egui::FontId::monospace(9.5);
    let label_col = contrast_color(pal.graph_bg);
    let [r, g, b, _] = pal.graph_bg.to_array();
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(r, g, b, 205),
    );
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, pal.grid.gamma_multiply(0.8)),
        egui::StrokeKind::Inside,
    );
    for (i, (label, color, dashed)) in entries.iter().enumerate() {
        let row_top = rect.top() + pad + row_h * i as f32;
        let cy = row_top + row_h * 0.5;
        let row_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left(), row_top),
            egui::pos2(rect.right(), row_top + row_h),
        );
        let resp = ui.interact(
            row_rect,
            ui.id().with(("legend-eye", label.as_str())),
            egui::Sense::click(),
        );
        if resp.clicked() && !hidden.remove(label) {
            hidden.insert(label.clone());
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let off = hidden.contains(label);
        let dim = |c: egui::Color32| if off { c.gamma_multiply(0.35) } else { c };
        // Eye glyph: open (round eye + pupil) when visible, a line when hidden.
        let eye = egui::pos2(rect.left() + pad + eye_w * 0.5, cy);
        if off {
            painter.line_segment(
                [egui::pos2(eye.x - 4.0, cy), egui::pos2(eye.x + 4.0, cy)],
                egui::Stroke::new(1.4, dim(label_col)),
            );
        } else {
            painter.circle_stroke(eye, 4.0, egui::Stroke::new(1.2, label_col));
            painter.circle_filled(eye, 1.6, label_col);
        }
        // Colour swatch.
        let x0 = rect.left() + pad + eye_w + gap;
        let x1 = x0 + sw;
        let stroke = egui::Stroke::new(2.0, dim(*color));
        if *dashed {
            painter.add(egui::Shape::dashed_line(
                &[egui::pos2(x0, cy), egui::pos2(x1, cy)],
                stroke,
                3.0,
                3.0,
            ));
        } else {
            painter.line_segment([egui::pos2(x0, cy), egui::pos2(x1, cy)], stroke);
        }
        painter.text(
            egui::pos2(x1 + gap, cy),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            dim(label_col),
        );
    }
}

fn nearest_band(
    state: &DaemonState,
    p: egui::Pos2,
    x_of: &dyn Fn(f64) -> f32,
    y_of: &dyn Fn(f64) -> f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, b) in state.bands.iter().enumerate() {
        // Disabled bands aren't drawn, so they must not be grabbable either —
        // otherwise a drag/double-click lands on an invisible node.
        if !b.enabled {
            continue;
        }
        let node = egui::pos2(x_of(curve::clampf_log(b.freq)), y_of(b.gain_db));
        let d = node.distance(p);
        if d < 14.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}
