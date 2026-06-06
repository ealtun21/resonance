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
            .with_min_inner_size([560.0, 400.0])
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Resonance",
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}

/// Procedurally drawn window/taskbar icon so the app never falls back to the
/// generic X11/Wayland placeholder. A dark rounded tile with three cyan EQ bars
/// — no asset file or image-decoder dependency needed.
fn app_icon() -> egui::IconData {
    const S: usize = 64;
    let mut rgba = vec![0u8; S * S * 4];
    let put = |buf: &mut [u8], x: usize, y: usize, c: [u8; 4]| {
        let i = (y * S + x) * 4;
        buf[i..i + 4].copy_from_slice(&c);
    };
    // Rounded-square dark background.
    let bg = [24u8, 27, 33, 255];
    let r = 10i32;
    for y in 0..S {
        for x in 0..S {
            let (xi, yi) = (x as i32, y as i32);
            let inside_x = xi >= r && xi < S as i32 - r;
            let inside_y = yi >= r && yi < S as i32 - r;
            let corner = |cx: i32, cy: i32| ((xi - cx).pow(2) + (yi - cy).pow(2)) <= r * r;
            let ok = inside_x
                || inside_y
                || corner(r, r)
                || corner(S as i32 - 1 - r, r)
                || corner(r, S as i32 - 1 - r)
                || corner(S as i32 - 1 - r, S as i32 - 1 - r);
            if ok {
                put(&mut rgba, x, y, bg);
            }
        }
    }
    // Three EQ bars of varying height in cyan/green.
    let bars = [
        (14usize, 40usize, [80u8, 200, 255, 255]),
        (28, 26, [70, 200, 120, 255]),
        (42, 46, [80, 200, 255, 255]),
    ];
    for (bx, bh, col) in bars {
        let top = S - 8 - bh;
        for y in top..S - 8 {
            for x in bx..bx + 8 {
                put(&mut rgba, x, y, col);
            }
        }
    }
    egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}
