//! Last-session autosave/restore: after a crash or restart the daemon comes
//! back with the state it last had (EQ, effects, IR, pinned output, ...).
//!
//! Stored as `<config_dir>/last-session.toml` — deliberately OUTSIDE
//! `profiles/`, so it never shows in `list_profiles` and can't collide with a
//! user profile name. Written atomically by a 1 s polling task (changes between
//! ticks coalesce into one write; unchanged state writes nothing) and flushed
//! once more on clean shutdown. Restore goes through `apply_profile_chain`, so
//! the IR and the Windows APO snapshot are handled exactly like a profile load.
//! A mapped profile for the active output still wins: it is applied later, when
//! the backend reports the output.
//!
//! Crash-loop guard: `last-session.attempts` counts restores that did not
//! survive `STABLE_AFTER`. At `MAX_ATTEMPTS` the session is quarantined
//! (`.bad`) and the daemon starts with defaults. Clean shutdown resets it.

use crate::config::{self, Profile};
use crate::state::SharedState;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

const VERSION: u32 = 1;
const MAX_ATTEMPTS: u32 = 3;
const STABLE_AFTER: Duration = Duration::from_secs(15);
const TICK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    #[serde(default)]
    version: u32,
    profile: Profile,
    /// Pinned output node name (`None` = follow the system default).
    #[serde(default)]
    pinned_output: Option<String>,
}

fn session_path(dir: &Path) -> PathBuf {
    dir.join("last-session.toml")
}
fn attempts_path(dir: &Path) -> PathBuf {
    dir.join("last-session.attempts")
}

fn capture(state: &SharedState) -> Session {
    let snap = state.snapshot();
    Session {
        version: VERSION,
        profile: Profile::from_state(&snap),
        pinned_output: snap.preferred_output,
    }
}

fn read_attempts(dir: &Path) -> u32 {
    std::fs::read_to_string(attempts_path(dir))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}
fn write_attempts(dir: &Path, n: u32) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(attempts_path(dir), n.to_string());
}

/// Parse a session file; any problem (corrupt, future version) is an `Err`.
fn parse(text: &str) -> Result<Session, String> {
    let s: Session = toml::from_str(text).map_err(|e| e.to_string())?;
    if s.version > VERSION {
        return Err(format!("written by a newer daemon (v{})", s.version));
    }
    Ok(s)
}

/// Load the session from `dir`, honouring the crash-loop guard. Bumps the
/// attempts counter when a session is returned. Never fails: bad input is
/// quarantined and `None` returned.
fn load(dir: &Path) -> Option<Session> {
    let path = session_path(dir);
    let text = std::fs::read_to_string(&path).ok()?;
    let attempts = read_attempts(dir);
    let verdict = if attempts >= MAX_ATTEMPTS {
        Err(format!("{attempts} consecutive unstable starts"))
    } else {
        parse(&text)
    };
    match verdict {
        Ok(s) => {
            write_attempts(dir, attempts + 1);
            Some(s)
        }
        Err(e) => {
            warn!("not restoring last session ({e}); starting with defaults");
            let _ = std::fs::rename(&path, path.with_extension("bad"));
            write_attempts(dir, 0);
            None
        }
    }
}

/// Apply the saved session (if any) to the freshly built daemon. Call once,
/// before the IPC socket is bound and before `spawn`.
pub fn restore(state: &SharedState) {
    let Some(s) = load(&config::config_dir()) else {
        return;
    };
    info!("restoring last session");
    crate::ipc_server::apply_profile_chain(&s.profile, state);
    // Windows pins by changing the OS default endpoint, which the OS persists
    // itself — never re-assert it behind the user's back at startup.
    #[cfg(not(target_os = "windows"))]
    if let Some(name) = s.pinned_output {
        let mut inner = state.0.lock().unwrap();
        // A missing device is harmless: the backend falls back to the default
        // sink and re-pins if the device reappears.
        let _ = inner.route_tx.send(name.clone());
        inner.preferred_output = Some(name);
    }
}

/// Change detector: yields serialised text only when it differs from the last
/// text written. Polling this at `TICK` is the debounce.
#[derive(Default)]
struct ChangeGate(Option<String>);

impl ChangeGate {
    fn next(&mut self, current: String) -> Option<String> {
        if self.0.as_deref() == Some(current.as_str()) {
            return None;
        }
        self.0 = Some(current.clone());
        Some(current)
    }
}

fn write(dir: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    config::write_atomic(&session_path(dir), text)
}

static HANDLE: std::sync::OnceLock<SharedState> = std::sync::OnceLock::new();

/// Start the autosave task. The file write runs on the blocking pool so the
/// IPC runtime never waits on disk.
pub fn spawn(state: &SharedState) {
    let _ = HANDLE.set(state.clone());
    let state = state.clone();
    tokio::spawn(async move {
        let dir = config::config_dir();
        let mut gate = ChangeGate::default();
        let started = tokio::time::Instant::now();
        let mut stable = false;
        let mut tick = tokio::time::interval(TICK);
        loop {
            tick.tick().await;
            if !stable && started.elapsed() >= STABLE_AFTER {
                stable = true;
                write_attempts(&dir, 0);
            }
            let Ok(text) = toml::to_string_pretty(&capture(&state)) else {
                continue;
            };
            if let Some(text) = gate.next(text) {
                let dir = dir.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = write(&dir, &text) {
                        warn!("could not autosave session: {e}");
                    }
                })
                .await;
            }
        }
    });
}

/// Final synchronous save on clean shutdown (also marks the run as stable).
pub fn flush() {
    let Some(state) = HANDLE.get() else { return };
    if state.0.is_poisoned() {
        return;
    }
    let dir = config::config_dir();
    if let Ok(text) = toml::to_string_pretty(&capture(state)) {
        if let Err(e) = write(&dir, &text) {
            warn!("could not save session on shutdown: {e}");
        }
    }
    write_attempts(&dir, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_ipc::EffectsState;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("resonance-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Session {
        Session {
            version: VERSION,
            profile: Profile {
                preamp_db: -3.5,
                enabled: true,
                effects: EffectsState::default(),
                bands: Vec::new(),
                dither_bits: Some(16),
                convolution: None,
                phase_mode: false,
            },
            pinned_output: Some("alsa_output.dac".into()),
        }
    }

    fn put(dir: &Path, s: &Session) {
        write(dir, &toml::to_string_pretty(s).unwrap()).unwrap();
    }

    #[test]
    fn round_trip() {
        let dir = tmp("rt");
        put(&dir, &sample());
        let s = load(&dir).expect("restored");
        assert!((s.profile.preamp_db + 3.5).abs() < 1e-9);
        assert_eq!(s.profile.dither_bits, Some(16));
        assert_eq!(s.pinned_output.as_deref(), Some("alsa_output.dac"));
        assert_eq!(read_attempts(&dir), 1);
    }

    #[test]
    fn corrupt_or_future_file_is_quarantined_not_fatal() {
        let dir = tmp("bad");
        write(&dir, "this is {{ not toml").unwrap();
        assert!(load(&dir).is_none());
        assert!(dir.join("last-session.bad").exists());
        assert!(!session_path(&dir).exists());

        let mut s = sample();
        s.version = VERSION + 1;
        put(&dir, &s);
        assert!(load(&dir).is_none());
    }

    #[test]
    fn missing_file_is_none() {
        assert!(load(&tmp("none")).is_none());
    }

    #[test]
    fn crash_loop_guard_skips_after_max_attempts() {
        let dir = tmp("loop");
        put(&dir, &sample());
        for _ in 0..MAX_ATTEMPTS {
            assert!(load(&dir).is_some(), "below the limit restores");
        }
        assert!(load(&dir).is_none(), "limit reached: defaults");
        assert_eq!(read_attempts(&dir), 0, "counter reset after quarantine");
    }

    #[test]
    fn change_gate_coalesces_and_skips_unchanged() {
        let mut g = ChangeGate::default();
        assert!(g.next("a".into()).is_some(), "first state is written");
        assert!(g.next("a".into()).is_none(), "unchanged writes nothing");
        // Several edits between two ticks arrive as one value: one write.
        assert_eq!(g.next("c".into()).as_deref(), Some("c"));
        assert!(g.next("c".into()).is_none());
    }

    #[test]
    fn session_is_outside_profiles_dir_and_name_cannot_collide() {
        let dir = tmp("list");
        assert!(!session_path(&dir).starts_with(dir.join("profiles")));
        // A user can't name a profile into the reserved dotted form.
        assert_eq!(config::sanitize_name(".last-session"), "last-session");
    }

    #[test]
    fn old_file_without_optional_fields_loads() {
        let toml = toml::to_string_pretty(&sample()).unwrap();
        let stripped: String = toml
            .lines()
            .filter(|l| !l.starts_with("pinned_output") && !l.starts_with("version"))
            .collect::<Vec<_>>()
            .join("\n");
        let s = parse(&stripped).expect("loads");
        assert_eq!(s.pinned_output, None);
    }
}
