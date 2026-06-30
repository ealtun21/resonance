//! Windows-only: per-application audio control via WASAPI audio sessions.
//!
//! Walks the default render endpoint's `IAudioSessionManager2` session list,
//! reads each session's process id, display name, volume and mute via
//! `IAudioSessionControl2` + `ISimpleAudioVolume`, and exposes set-volume /
//! set-mute. The daemon owns no audio backend on Windows (the APO does the DSP
//! in `audiodg.exe`), so per-app control is a pure control-plane operation here,
//! driven from dedicated COM threads.
//!
//! Note: `ISimpleAudioVolume` is a 0.0–1.0 scalar — Windows has no native
//! per-session boost, so requested volumes above unity are clamped to 1.0.

// COM interface methods are PascalCase.
#![allow(non_snake_case)]

use super::app_streams::{app_key, exe_basename_from_session_id};
use crate::state::AppControl;
use resonance_ipc::AppStream;
use std::time::Duration;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::core::Interface;

/// Run `f` with the default render endpoint's session enumerator. Initialises
/// COM on the calling thread (re-init is harmless: returns `S_FALSE`).
fn with_sessions<T>(
    f: impl FnOnce(&IAudioSessionEnumerator) -> windows::core::Result<T>,
) -> windows::core::Result<T> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None)?;
        let sessions = mgr.GetSessionEnumerator()?;
        f(&sessions)
    }
}

/// Stable key + display name for a session, parsed from its instance identifier
/// (which embeds the exe path) with the pid as the serial. Frees the COM string.
unsafe fn session_identity(ctrl: &IAudioSessionControl2, pid: u32) -> (String, String) {
    let exe = unsafe {
        match ctrl.GetSessionInstanceIdentifier() {
            Ok(pw) if !pw.is_null() => {
                let s = pw.to_string().unwrap_or_default();
                CoTaskMemFree(Some(pw.0.cast()));
                exe_basename_from_session_id(&s)
            }
            _ => None,
        }
    };
    let display = exe.clone().unwrap_or_else(|| format!("PID {pid}"));
    let key = app_key(exe.as_deref(), &display, pid);
    (key, display)
}

/// Enumerate the active render endpoint's application sessions. Skips the
/// system-sounds session. Returns an empty vec if COM/enumeration fails.
#[must_use]
pub fn enumerate() -> Vec<AppStream> {
    with_sessions(|sessions| {
        let count = unsafe { sessions.GetCount()? };
        let mut apps = Vec::new();
        for i in 0..count {
            if let Ok(Some(app)) = unsafe { session_to_app(sessions, i) } {
                apps.push(app);
            }
        }
        Ok(apps)
    })
    .unwrap_or_default()
}

unsafe fn session_to_app(
    sessions: &IAudioSessionEnumerator,
    i: i32,
) -> windows::core::Result<Option<AppStream>> {
    unsafe {
        let ctrl = sessions.GetSession(i)?;
        let ctrl2: IAudioSessionControl2 = ctrl.cast()?;
        // Skip the OS system-sounds session (IsSystemSoundsSession returns S_OK).
        if ctrl2.IsSystemSoundsSession() == S_OK {
            return Ok(None);
        }
        let pid = ctrl2.GetProcessId()?;
        let vol: ISimpleAudioVolume = ctrl.cast()?;
        let volume = f64::from(vol.GetMasterVolume()?);
        let muted = vol.GetMute()?.as_bool();
        let active = ctrl.GetState()? == AudioSessionStateActive;
        let (key, display_name) = session_identity(&ctrl2, pid);
        Ok(Some(AppStream {
            key,
            display_name,
            pid: Some(pid),
            volume,
            muted,
            active,
        }))
    }
}

/// Find the session matching `key` and apply `f` to its `ISimpleAudioVolume`.
fn apply_to_session(
    key: &str,
    f: impl Fn(&ISimpleAudioVolume) -> windows::core::Result<()>,
) -> bool {
    with_sessions(|sessions| {
        let count = unsafe { sessions.GetCount()? };
        for i in 0..count {
            let matched = unsafe {
                let ctrl = sessions.GetSession(i)?;
                let ctrl2: IAudioSessionControl2 = ctrl.cast()?;
                if ctrl2.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                let pid = ctrl2.GetProcessId()?;
                let (skey, _) = session_identity(&ctrl2, pid);
                if skey == key {
                    let vol: ISimpleAudioVolume = ctrl.cast()?;
                    f(&vol)?;
                    true
                } else {
                    false
                }
            };
            if matched {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .unwrap_or(false)
}

/// Set a session's volume (clamped to 0.0–1.0; Windows has no native boost).
pub fn set_volume(key: &str, volume: f64) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let level = volume.clamp(0.0, 1.0) as f32;
    apply_to_session(key, |vol| unsafe {
        vol.SetMasterVolume(level, std::ptr::null())
    })
}

/// Mute or unmute a session.
pub fn set_mute(key: &str, muted: bool) -> bool {
    apply_to_session(key, |vol| unsafe { vol.SetMute(muted, std::ptr::null()) })
}

/// Spawn the Windows per-app control plane: a thread that polls the session list
/// and publishes it, and a thread that applies incoming volume/mute requests.
/// Both own COM (via `with_sessions`); they run for the daemon's lifetime.
pub fn spawn_app_tasks(
    apps_tx: tokio::sync::mpsc::UnboundedSender<Vec<AppStream>>,
    app_ctl_rx: std::sync::mpsc::Receiver<AppControl>,
) {
    std::thread::Builder::new()
        .name("resonance-win-apps".into())
        .spawn(move || {
            loop {
                if apps_tx.send(enumerate()).is_err() {
                    break; // daemon shutting down
                }
                std::thread::sleep(Duration::from_millis(1500));
            }
        })
        .ok();

    std::thread::Builder::new()
        .name("resonance-win-appctl".into())
        .spawn(move || {
            while let Ok(ctl) = app_ctl_rx.recv() {
                match ctl {
                    AppControl::SetVolume { key, volume } => {
                        let _ = set_volume(&key, volume);
                    }
                    AppControl::SetMute { key, muted } => {
                        let _ = set_mute(&key, muted);
                    }
                }
            }
        })
        .ok();
}
