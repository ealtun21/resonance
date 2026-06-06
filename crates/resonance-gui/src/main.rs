//! `resonance-gui` — egui/eframe desktop client for the Resonance daemon.

mod app;
mod browser;
mod curve;
mod icon;
mod ipc;
mod theme;

use app::GuiApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Resonance")
            // Floor only — the real minimum width is computed at runtime from the
            // toolbar's measured content and pushed via `MinInnerSize`, so it
            // adapts to font/scale/device-name instead of a hardcoded value.
            .with_inner_size([1240.0, 760.0])
            .with_min_inner_size([600.0, 460.0])
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Resonance",
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}

/// Window/taskbar icon, rasterised from the shared brand drawing (matches
/// `contrib/io.github.ealtun21.Resonance.svg`) so it never falls back to the
/// generic X11/Wayland placeholder — no image-decoder dependency needed.
fn app_icon() -> egui::IconData {
    const S: u32 = 128;
    egui::IconData {
        rgba: icon::rgba(S as usize),
        width: S,
        height: S,
    }
}
