//! `resonance-gui` — egui/eframe desktop client for the Resonance daemon.

mod app;
mod browser;
mod curve;
mod ipc;
mod theme;

use app::GuiApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Resonance")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([560.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Resonance",
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}
