//! macOS environment control: CoreAudio devices via the `audiodev` helper,
//! the daemon via launchd (the TCC grant belongs to the launchd-started
//! process, not to a child of the agent).

use anyhow::{Context, Result, ensure};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub const DAEMON_LABEL: &str = "e2e.resonanced";
pub const DAEMON_LOG: &str = "/tmp/e2e-daemon.log";

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("spawn {cmd:?}"))?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    ensure!(out.status.success(), "{cmd:?} failed: {text}");
    Ok(text)
}

/// Parse `audiodev list` lines (`id\tname\tin=N\tout=N\trate`) into
/// `(name, in_channels, out_channels, rate)`.
#[must_use]
pub fn parse_device_list(text: &str) -> Vec<(String, usize, usize, u32)> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let num = |s: &str, key: &str| s.strip_prefix(key)?.parse::<usize>().ok();
            Some((
                f.get(1)?.to_string(),
                num(f.get(2)?, "in=")?,
                num(f.get(3)?, "out=")?,
                f.get(4)?.parse::<f64>().ok()? as u32,
            ))
        })
        .collect()
}

pub fn audiodev(dir: &Path, args: &[&str]) -> Result<String> {
    run(Command::new(dir.join("audiodev")).args(args))
}

/// Put both BlackHole devices at `rate` and make `input` the default output.
pub fn set_devices(dir: &Path, input: &str, output: &str, rate: u32) -> Result<()> {
    for d in [input, output] {
        audiodev(dir, &["rate", d, &rate.to_string()])?;
    }
    audiodev(dir, &["default", input])?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn uid() -> String {
    // SAFETY: getuid has no preconditions.
    unsafe { getuid() }.to_string()
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getuid() -> u32;
}

/// Only meaningful on macOS (this module's pure parts are shared for tests).
#[cfg(not(target_os = "macos"))]
fn uid() -> String {
    "0".into()
}

fn target() -> String {
    format!("gui/{}/{DAEMON_LABEL}", uid())
}

/// Start the daemon through launchd and wait for its IPC socket.
pub fn start_daemon() -> Result<()> {
    let _ = std::fs::remove_file(DAEMON_LOG);
    run(Command::new("launchctl").args(["kickstart", "-k", &target()]))?;
    let end = Instant::now() + Duration::from_secs(20);
    while !resonance_ipc::transport::is_reachable() {
        ensure!(
            Instant::now() < end,
            "daemon socket never appeared; log: {}",
            std::fs::read_to_string(DAEMON_LOG).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

pub fn stop_daemon() {
    let _ = Command::new("launchctl")
        .args(["kill", "TERM", &target()])
        .status();
    let end = Instant::now() + Duration::from_secs(10);
    while resonance_ipc::transport::is_reachable() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_audiodev_listing() {
        let l = parse_device_list(
            "73\tBlackHole 2ch\tin=2\tout=2\t48000\n74\tBlackHole 64ch\tin=64\tout=64\t96000\nbad line\n",
        );
        assert_eq!(l.len(), 2);
        assert_eq!(l[1], ("BlackHole 64ch".to_string(), 64, 64, 96_000));
    }
}
