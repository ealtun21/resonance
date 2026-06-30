//! `resonance-gui` — egui/eframe desktop client for the Resonance daemon.

// On Windows, run as a GUI-subsystem process so no console window appears
// behind the app. `--dump-iconset` (used by CI) still works — it writes files.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod browser;
mod curve;
mod icon;
mod ipc;
mod state;
mod theme;
mod ui;

use app::GuiApp;

fn main() -> eframe::Result<()> {
    // Build-time helper: `resonance-gui --dump-iconset <dir>` writes the
    // 10 PNG variants Apple's iconutil needs into <dir>, then exits. The
    // build script (contrib/macos/build-app.sh) calls this so the icon
    // is rendered by our own rasteriser (matches the SVG at every size)
    // instead of qlmanage, which pads small SVGs into the top-left of a
    // larger canvas.
    let args: Vec<String> = std::env::args().collect();
    if let Some(idx) = args.iter().position(|a| a == "--dump-iconset") {
        let dir = args
            .get(idx + 1)
            .expect("--dump-iconset requires a path argument");
        dump_iconset(std::path::Path::new(dir));
        return Ok(());
    }

    // Dev helper: `RESONANCE_ICON_GALLERY=1` opens a window that just paints the
    // vector icon set in a labelled grid, so the shapes can be eyeballed via the
    // screenshot harness. No daemon, no real UI.
    if std::env::var_os("RESONANCE_ICON_GALLERY").is_some() {
        return run_icon_gallery();
    }

    // If the user launched the GUI without a daemon (which is the default
    // when launching the .app bundle from Launchpad / Spotlight), spawn
    // one in the BACKGROUND so the window comes up instantly. The UI
    // already handles "no daemon" gracefully (shows a disconnected
    // screen), so we don't block the main thread waiting for the socket.
    std::thread::Builder::new()
        .name("resonance-daemon-spawner".into())
        .spawn(ensure_daemon_running)
        .expect("spawn daemon supervisor thread");

    // Native OS window decorations on every platform. The Windows native title
    // bar is colour-themed at runtime via DWM (see app::native_titlebar). The
    // old custom client-side titlebar was removed — a decoration-less winit
    // window had unreliable client-area input (clicks/drags landing off-target).
    // Dev/test override: `RESONANCE_WINDOW_SIZE=WxH` opens the window at an exact
    // inner size pinned to the top-left, so the screenshot harness
    // (contrib/dev/uishot.sh) can capture the UI at arbitrary widths under Xvfb.
    // Absent in normal use — zero effect on the shipped app.
    let forced_size = std::env::var("RESONANCE_WINDOW_SIZE")
        .ok()
        .and_then(|s| parse_wxh(&s));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Resonance")
        .with_decorations(true)
        .with_inner_size(forced_size.unwrap_or([1240.0, 760.0]))
        // Small-window friendly: below ~1000px the lower sections collapse to
        // a single-column accordion and the toolbar wraps, so a low floor is
        // fine — no runtime min-width computation needed.
        .with_min_inner_size([360.0, 420.0])
        .with_icon(std::sync::Arc::new(app_icon()));
    if forced_size.is_some() {
        viewport = viewport.with_position([0.0, 0.0]);
    }
    // macOS: a unified title bar — the content (our toolbar) draws under a
    // transparent title bar so the window chrome blends into the app instead of
    // showing a separate grey bar. The traffic-light buttons float over the
    // toolbar, which reserves space for them (see `toolbar()`). Windows blends
    // its title bar via DWM caption colours instead (see app::native_titlebar).
    #[cfg(target_os = "macos")]
    let viewport = viewport
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Resonance",
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}

/// Ensure the `resonanced` daemon is running.
///
/// Strategy:
///   1. If the socket already responds, nothing to do.
///   2. Otherwise, if the platform service manager is reachable, ask IT
///      to start the daemon. We do NOT fall back to a direct spawn when
///      `service::start()` succeeds — the spawned daemon takes the
///      single-instance pidfile, which then blocks the service manager
///      from ever starting its own (the daemon refuses to boot when
///      another instance owns the pidfile). Mixing the two control
///      paths leaves the GUI's daemon panel permanently stuck on
///      "stopped" because launchd never gets to spawn the real daemon.
///   3. Only when the service manager isn't reachable (e.g. cargo-run in
///      a container) do we fall back to a direct detached spawn.
///
/// The GUI shows the "disconnected" screen while the daemon comes up —
/// the IPC reconnect loop polls the socket and switches into the main
/// view as soon as it's reachable.
fn ensure_daemon_running() {
    if resonance_ipc::transport::is_reachable() {
        return;
    }

    if resonance_ipc::service::manager_available() {
        match resonance_ipc::service::start() {
            Ok(()) => {
                // Service manager has taken ownership. The daemon's audio init
                // can take 1–3 s on cold start; we don't block on it — the
                // GUI's reconnect loop sees the daemon as soon as it's bound.
            }
            Err(e) => {
                eprintln!("GUI: service manager start failed ({e}); falling back to direct spawn");
                spawn_daemon_detached();
            }
        }
        return;
    }

    // No service manager: spawn directly (cargo-run / container path).
    spawn_daemon_detached();
}

/// Block (briefly) until the daemon is reachable, or until `timeout`.
fn wait_for_daemon(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if resonance_ipc::transport::is_reachable() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// Fallback when no service manager is available. Spawns the daemon as a
/// detached child with logs going to the platform's log dir.
fn spawn_daemon_detached() {
    // Same resolution the service layer uses (sibling of this exe, then $PATH,
    // then the bare name); a missing binary surfaces as a spawn error below.
    let daemon_path = resonance_ipc::service::daemon_bin();
    let log_path = resonance_ipc::paths::daemon_log_path();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    eprintln!(
        "GUI: spawning {} (logs → {})",
        daemon_path.display(),
        log_path.display()
    );
    let mut cmd = std::process::Command::new(&daemon_path);
    cmd.env(
        "RUST_LOG",
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    );
    if let Some(log) = log {
        let log_dup = log.try_clone().ok();
        cmd.stdout(std::process::Stdio::from(log));
        if let Some(dup) = log_dup {
            cmd.stderr(std::process::Stdio::from(dup));
        }
    }
    match cmd.spawn() {
        Ok(child) => {
            // Reap the child when it eventually exits so it doesn't linger as a
            // zombie for the GUI's lifetime. This fallback path has no service
            // manager to own the process, so the GUI must.
            std::thread::Builder::new()
                .name("resonance-daemon-reaper".into())
                .spawn(move || {
                    let mut child = child;
                    let _ = child.wait();
                })
                .ok();
            if !wait_for_daemon(std::time::Duration::from_secs(2)) {
                eprintln!("GUI: daemon spawned but not reachable yet — GUI will retry");
            }
        }
        Err(e) => {
            eprintln!("GUI: failed to spawn {}: {e}", daemon_path.display());
        }
    }
}

/// Run the dev icon gallery (see the `RESONANCE_ICON_GALLERY` check in `main`).
#[allow(deprecated)] // top-level CentralPanel::show in a one-off dev helper
fn run_icon_gallery() -> eframe::Result<()> {
    let forced_size = std::env::var("RESONANCE_WINDOW_SIZE")
        .ok()
        .and_then(|s| parse_wxh(&s));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Resonance — icons")
        .with_inner_size(forced_size.unwrap_or([720.0, 560.0]));
    if forced_size.is_some() {
        viewport = viewport.with_position([0.0, 0.0]);
    }
    eframe::run_simple_native(
        "Resonance icons",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        |ctx, _frame| {
            egui::CentralPanel::default().show(ctx, ui::icons::gallery);
        },
    )
}

/// Parse a `WIDTHxHEIGHT` string (e.g. `480x700`) into an egui inner-size pair.
/// Used only by the `RESONANCE_WINDOW_SIZE` dev/test override.
fn parse_wxh(s: &str) -> Option<[f32; 2]> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
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

/// Render the 10 PNGs Apple's iconutil expects for a `.iconset` and write
/// them to `dir`. Each rendering uses our pure-Rust rasteriser
/// (`icon::rgba`) so the small variants stay sharp at native resolution
/// instead of being scaled up from a 128px qlmanage thumbnail.
fn dump_iconset(dir: &std::path::Path) {
    use image::{ImageBuffer, Rgba};
    std::fs::create_dir_all(dir).expect("create iconset dir");
    // (pixel size, filename Apple expects)
    let sizes: &[(u32, &str)] = &[
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ];
    for (size, name) in sizes {
        let rgba = icon::rgba(*size as usize);
        let img: ImageBuffer<Rgba<u8>, _> =
            ImageBuffer::from_raw(*size, *size, rgba).expect("rgba length matches size");
        let out = dir.join(name);
        img.save(&out).unwrap_or_else(|e| {
            eprintln!("failed to write {}: {e}", out.display());
            std::process::exit(1);
        });
        println!("wrote {} ({size}×{size})", out.display());
    }
}
