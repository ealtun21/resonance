//! Windows environment control: endpoint format, daemon process, APO log.

use crate::common::wait_for;
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The APO's own log (written by audiodg), one line per lifecycle event.
pub fn apo_log_path() -> PathBuf {
    let pd = std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), Into::into);
    pd.join("Resonance").join("apo.log")
}

/// Current length of the APO log, to read only what a run appended.
pub fn apo_log_mark() -> u64 {
    std::fs::metadata(apo_log_path()).map_or(0, |m| m.len())
}

pub fn apo_log_since(mark: u64) -> String {
    let all = std::fs::read(apo_log_path()).unwrap_or_default();
    String::from_utf8_lossy(
        all.get(usize::try_from(mark).unwrap_or(0)..)
            .unwrap_or_default(),
    )
    .into_owned()
}

/// "APO on" evidence in `log`: audiodg locked the APO at this format and then
/// processed buffers with the daemon's state enabled. Pure, so it is unit
/// tested; the live run feeds it the lines appended during playback.
pub fn verify_apo_on(log: &str, channels: usize, rate: u32) -> Result<()> {
    let lock = format!("LockForProcess hr=0x00000000 ch={channels} rate={rate}");
    let lines: Vec<&str> = log.lines().collect();
    let last_lock = lines
        .iter()
        .rposition(|l| l.contains(&lock))
        .with_context(|| {
            format!("no `{lock}` in the APO log (APO not instantiated for this format)")
        })?;
    let processed = lines[last_lock..].iter().any(|l| {
        l.contains("process #")
            && l.contains(&format!("ch={channels}"))
            && l.contains("enabled=true")
    });
    ensure!(
        processed,
        "APO locked at {channels} ch / {rate} Hz but logged no processed buffer with enabled=true"
    );
    Ok(())
}

/// Run a script from `contrib/e2e/windows` (admin token inherited from the session).
fn script(dir: &Path, name: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(dir.join(name))
        .args(args)
        .output()
        .with_context(|| format!("run {name}"))?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    eprintln!("e2e: {name}: {}", text.trim());
    ensure!(out.status.success(), "{name} failed: {text}");
    Ok(text)
}

/// Make `which` the only present playback device (so it is the default).
/// `force` re-enables it even when already selected (recovery).
pub fn select_endpoint(scripts: &Path, which: super::Endpoint, force: bool) -> Result<()> {
    let arg = match which {
        super::Endpoint::Hda => "hda",
        super::Endpoint::Scream => "scream",
    };
    let mut args = vec!["-Which", arg];
    if force {
        args.push("-Force");
    }
    let out = script(scripts, "select-endpoint.ps1", &args)?;
    if !out.contains("already") {
        // The audio services come back asynchronously.
        std::thread::sleep(Duration::from_secs(4));
    }
    Ok(())
}

/// Set the endpoint's shared-mode format (restarts the audio services when it changes).
pub fn set_endpoint_format(scripts: &Path, channels: usize, rate: u32) -> Result<()> {
    // Only Scream's format is configurable; the HDA device is fixed at 2 ch / 48 kHz.
    if channels != 8 {
        return Ok(());
    }
    script(
        scripts,
        "setfmt.ps1",
        &["-Ch", &channels.to_string(), "-Rate", &rate.to_string()],
    )?;
    // The audio services come back asynchronously.
    std::thread::sleep(Duration::from_secs(3));
    Ok(())
}

/// Attach the APO to the endpoint named `name` in `slot`, or detach it (`None`), and restart
/// the audio services (the engine rebuilds its graph).
pub fn set_apo_slot(scripts: &Path, name: &str, slot: Option<u8>) -> Result<()> {
    let slot = slot.map_or_else(|| "none".to_string(), |s| s.to_string());
    script(scripts, "attach-slot.ps1", &["-Slot", &slot, "-Name", name])?;
    std::thread::sleep(Duration::from_secs(4));
    Ok(())
}

/// "APO off" evidence in `log`: audiodg never locked an APO during the run. Pure.
pub fn verify_apo_absent(log: &str) -> Result<()> {
    ensure!(
        !log.contains("LockForProcess"),
        "the APO was instantiated during a Resonance-off measurement"
    );
    Ok(())
}

pub struct Daemon {
    child: Child,
}

impl Daemon {
    pub fn start(bin: &Path, log: &Path) -> Result<Self> {
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        let child = Command::new(bin)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("start {}", bin.display()))?;
        let d = Self { child };
        wait_for("daemon IPC", Duration::from_secs(15), || {
            Ok(resonance_ipc::transport::is_reachable())
        })?;
        Ok(d)
    }

    pub fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "pid=1 cpp: base LockForProcess hr=0x00000000 ch=2 rate=48000 maxFrames=480\n\
        pid=1 process #0: in_peak=0.0 in_rms=0.0 frames=480 ch=2 enabled=false preamp=0.0 filters=0\n\
        pid=2 cpp: base LockForProcess hr=0x00000000 ch=8 rate=192000 maxFrames=1920\n\
        pid=2 process #0: in_peak=0.2 in_rms=0.1 frames=1920 ch=8 enabled=true preamp=0.0 filters=3\n";

    #[test]
    fn accepts_a_lock_followed_by_an_enabled_buffer() {
        assert!(verify_apo_on(LOG, 8, 192_000).is_ok());
    }

    #[test]
    fn absence_means_no_lock_line() {
        assert!(verify_apo_absent("pid=1 cpp: Initialize cbDataSize=56\n").is_ok());
        assert!(verify_apo_absent(LOG).is_err());
    }

    #[test]
    fn rejects_a_missing_lock_and_a_disabled_apo() {
        assert!(verify_apo_on(LOG, 8, 48_000).is_err());
        assert!(verify_apo_on(LOG, 2, 48_000).is_err(), "enabled=false only");
    }
}
