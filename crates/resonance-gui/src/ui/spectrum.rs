//! Animated spectrum bars (bottom panel).

use crate::app::GuiApp;
use crate::ui::widgets::lerp_color;
use eframe::egui;
use resonance_ipc::DaemonState;
use std::time::Instant;

/// Spectrum envelope time constants: bars snap up, glide down.
const SPECTRUM_ATTACK_TAU: f32 = 0.020;
const SPECTRUM_DECAY_TAU: f32 = 0.20;

impl GuiApp {
    // ── Spectrum bars ───────────────────────────────────────────────────────

    pub(crate) fn spectrum(&mut self, ui: &mut egui::Ui, state: &DaemonState) {
        // Fill the resizable spectrum panel rather than a fixed height (a fixed
        // height taller than the panel makes the splitter bounce back).
        let height = ui.available_height().max(16.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

        let bins = &state.spectrum;
        if bins.is_empty() {
            return;
        }
        let n = bins.len();

        // Smooth each bar toward the latest value: fast rise, slow fall. This
        // is what kills the flicker — the data jumps, the bars don't.
        let dt = self.last_anim.elapsed().as_secs_f32().min(0.1);
        self.last_anim = Instant::now();
        if self.spectrum_display.len() != n {
            self.spectrum_display = vec![0.0; n];
        }
        for (disp, &raw) in self.spectrum_display.iter_mut().zip(bins.iter()) {
            let target = raw.clamp(0.0, 1.0);
            let tau = if target > *disp {
                SPECTRUM_ATTACK_TAU
            } else {
                SPECTRUM_DECAY_TAU
            };
            let coeff = 1.0 - (-dt / tau).exp();
            *disp += (target - *disp) * coeff;
        }

        let gap = 2.0;
        let bw = (rect.width() - gap * (n as f32 + 1.0)) / n as f32;
        let pal = self.palette;
        for (i, &v) in self.spectrum_display.iter().enumerate() {
            let h = (v.clamp(0.0, 1.0)) * (rect.height() - 4.0);
            let x0 = rect.left() + gap + i as f32 * (bw + gap);
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0, rect.bottom() - 2.0 - h),
                egui::pos2(x0 + bw, rect.bottom() - 2.0),
            );
            // Theme-aware gradient: low energy = accent, peaks toward highlight.
            let t = v.clamp(0.0, 1.0);
            let color = lerp_color(pal.accent, pal.highlight, t);
            painter.rect_filled(bar, 1.0, color);
        }
    }
}
