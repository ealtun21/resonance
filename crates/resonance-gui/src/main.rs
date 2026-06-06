//! `resonance-gui` — egui/eframe desktop client for the Resonance daemon.

mod app;
mod browser;
mod curve;
mod icon;
mod ipc;
mod theme;

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

    // If the user launched the GUI without a daemon (which is the default
    // when launching the .app bundle from Launchpad / Spotlight), spawn
    // one in the BACKGROUND so the window comes up instantly. The UI
    // already handles "no daemon" gracefully (shows a disconnected
    // screen), so we don't block the main thread waiting for the socket.
    std::thread::Builder::new()
        .name("resonance-daemon-spawner".into())
        .spawn(ensure_daemon_running)
        .expect("spawn daemon supervisor thread");

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

/// Ensure the `resonanced` daemon is running.
///
/// Strategy:
///   1. If the socket already responds, nothing to do.
///   2. Otherwise, if the platform service manager is reachable, ask IT
///      to start the daemon. We do NOT fall back to a direct spawn when
///      service::start() succeeds — the spawned daemon takes the
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
    let socket = resonance_ipc::paths::default_socket_path();
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return;
    }

    if resonance_ipc::service::systemd_available() {
        match resonance_ipc::service::start() {
            Ok(()) => {
                // Service manager has taken ownership. The daemon's
                // CoreAudio init can take 1–3 s on cold start; we don't
                // block on it — the GUI's reconnect loop sees the socket
                // as soon as it's bound.
            }
            Err(e) => {
                eprintln!(
                    "GUI: service manager start failed ({e}); falling back to direct spawn"
                );
                spawn_daemon_detached(&socket);
            }
        }
        return;
    }

    // No service manager: spawn directly (cargo-run / container path).
    spawn_daemon_detached(&socket);
}

/// Block (briefly) until the daemon binds its socket, or until `timeout`.
fn wait_for_socket(socket: &std::path::Path, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// Fallback when no service manager is available. Spawns the daemon as a
/// detached child with logs going to the platform's log dir.
fn spawn_daemon_detached(socket: &std::path::Path) {
    let Some(daemon_path) = locate_daemon() else {
        eprintln!(
            "GUI: could not locate resonanced binary near this executable; \
             start it manually with `resonanced` and relaunch"
        );
        return;
    };
    let log_path = {
        #[cfg(target_os = "macos")]
        {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let dir = std::path::PathBuf::from(home).join("Library/Logs/resonance");
            let _ = std::fs::create_dir_all(&dir);
            dir.join("resonanced.log")
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::path::PathBuf::from("/tmp/resonanced.log")
        }
    };
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
        Ok(_child) => {
            if !wait_for_socket(socket, std::time::Duration::from_secs(2)) {
                eprintln!(
                    "GUI: daemon spawned but socket {} not yet ready — GUI will retry",
                    socket.display()
                );
            }
        }
        Err(e) => {
            eprintln!("GUI: failed to spawn {}: {e}", daemon_path.display());
        }
    }
}

/// Find the `resonanced` binary. Searches, in order:
///   1. Same directory as the GUI executable (covers bundle + `cargo run`
///      from `target/release` or `target/debug`).
///   2. `$PATH` lookup.
///   3. The literal name `resonanced`, letting `Command::spawn` fall back
///      to the shell's resolution.
fn locate_daemon() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("resonanced");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(':').filter(|s| !s.is_empty()) {
            let cand = std::path::Path::new(d).join("resonanced");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    Some(std::path::PathBuf::from("resonanced"))
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
