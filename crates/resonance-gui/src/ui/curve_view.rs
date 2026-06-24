//! The EQ response curve panel: draggable band nodes, zoom, and the
//! auto-scaling dB axis.

use crate::app::GuiApp;
use crate::curve;
use crate::state::{GAIN_LIMIT, Q_LIMIT};
use crate::ui::widgets::{contrast_color, gain_color, lerp_color};
use eframe::egui;
use resonance_ipc::{BandType, Command, DaemonState};
use std::time::Instant;

/// Spectrum envelope time constants: bars snap up, glide down (drawn behind the
/// response curve so silence shows no fill — never a separate black panel).
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.20;

impl GuiApp {
    // ── EQ response curve (draggable nodes) ─────────────────────────────────

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
        // Loudest point the axis must show (any band gain or curve peak) + 5 dB
        // headroom. Includes the dragged band, so the axis expands live as you
        // drag a node up and contracts as you bring it down.
        let peak = pts
            .iter()
            .map(|&(_, g)| g.abs())
            .chain(bands.iter().map(|b| b.gain_db.abs()))
            .fold(0.0_f64, f64::max);
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
                const MIN_BODY: f32 = 0.018;
                for (i, &v) in self.spectrum_display.iter().enumerate() {
                    let v = v.clamp(0.0, 1.0);
                    let lo = curve::LOG_MIN + i as f64 / n as f64 * range;
                    let hi = curve::LOG_MIN + (i + 1) as f64 / n as f64 * range;
                    let x0 = x_of(lo).max(plot.left());
                    let x1 = x_of(hi).min(plot.right());
                    if x1 <= x0 {
                        continue;
                    }
                    let body = (v * 0.92 + MIN_BODY).min(1.0);
                    let top_y = base - body * plot.height();
                    let col = lerp_color(pal.accent, pal.highlight, v);
                    let [r, g, b, _] = col.to_array();
                    let a = (45.0 + 175.0 * v).min(225.0) as u8;
                    painter.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(x0, top_y), egui::pos2(x1, base)),
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(r, g, b, a),
                    );
                }
            }
        }

        // Response curve — colour-coded by gain: each segment is tinted toward
        // boost (green) or cut (red), neutral near 0 dB.
        for w in pts.windows(2) {
            let (lf0, g0) = w[0];
            let (lf1, g1) = w[1];
            let a = egui::pos2(x_of(lf0), y_of(g0));
            let b = egui::pos2(x_of(lf1), y_of(g1));
            let color = gain_color((g0 + g1) * 0.5, &pal);
            painter.line_segment([a, b], egui::Stroke::new(2.0, color));
        }

        use egui::PointerButton::{Primary, Secondary};

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
        if response.double_clicked_by(Secondary) {
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
        if (started_primary || started_secondary) && self.zoom_sel.is_none() {
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
        if response.double_clicked_by(Primary) {
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
