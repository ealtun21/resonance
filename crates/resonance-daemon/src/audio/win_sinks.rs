//! Windows-only: per-output-sink (render endpoint) volume/mute via WASAPI.
//!
//! Mirrors [`super::win_apps`] but for *output devices* rather than app
//! sessions: enumerates the active `eRender` endpoints, reads each endpoint's
//! stable id + friendly name + master volume/mute via `IAudioEndpointVolume`,
//! and exposes set-volume / set-mute keyed by the endpoint id. The daemon owns
//! no audio backend on Windows (the APO does the DSP in `audiodg.exe`), so
//! per-sink control is a pure control-plane operation here, driven from
//! dedicated COM threads — the analog of `PipeWire`'s Device Route params.
//!
//! `IAudioEndpointVolume::GetMasterVolumeLevelScalar`/`SetMasterVolumeLevelScalar`
//! is a **perceptual** 0.0–1.0 scalar — exactly the range the Windows volume
//! slider uses and the range `SinkVolume::volume` carries — so there is NO cube
//! conversion here (unlike the Linux backend, whose `PipeWire` `channelVolumes`
//! is linear and must be cube-rooted). The endpoint scalar caps at 1.0, so
//! requested volumes above unity are clamped to 1.0.

// COM interface methods are PascalCase.
#![allow(non_snake_case)]

use resonance_ipc::SinkVolume;

use crate::state::SinkCtl;
use std::time::Duration;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::Win32::System::Variant::VT_LPWSTR;

/// Read a friendly-name PROPVARIANT as an owned String, guarding the union: only
/// `VT_LPWSTR` with a non-null pointer is a valid wide string (any other variant
/// or a null pointer would be a wild deref). Returns "" otherwise. Mirrors
/// `win_devices::propvariant_str` (kept local so this module has no cross-module
/// coupling to a private helper).
///
/// SAFETY: caller passes a PROPVARIANT obtained from `IPropertyStore::GetValue`.
unsafe fn propvariant_str(prop: &PROPVARIANT) -> String {
    unsafe {
        let v = &prop.Anonymous.Anonymous;
        if v.vt == VT_LPWSTR {
            let p = v.Anonymous.pwszVal;
            if !p.is_null() {
                return p.to_string().unwrap_or_default();
            }
        }
        String::new()
    }
}

/// Run `f` with the render-endpoint collection (active `eRender` devices).
/// Initialises COM on the calling thread (re-init is harmless: `S_FALSE`).
fn with_render_endpoints<T>(
    f: impl FnOnce(&IMMDeviceEnumerator) -> windows::core::Result<T>,
) -> windows::core::Result<T> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        f(&enumerator)
    }
}

/// The stable WASAPI endpoint id for `dev` (e.g.
/// `{0.0.0.00000000}.{<guid>}`), or "" on failure. Frees the COM string.
unsafe fn endpoint_id(dev: &IMMDevice) -> String {
    unsafe {
        match dev.GetId() {
            Ok(id) if !id.is_null() => {
                let s = id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id.0 as *const _));
                s
            }
            _ => String::new(),
        }
    }
}

/// The endpoint's friendly name (e.g. "Speakers (Realtek Audio)"), or "".
unsafe fn endpoint_friendly_name(dev: &IMMDevice) -> String {
    unsafe {
        (|| -> windows::core::Result<String> {
            let store = dev.OpenPropertyStore(STGM_READ)?;
            let prop = store.GetValue(&PKEY_Device_FriendlyName)?;
            Ok(propvariant_str(&prop))
        })()
        .unwrap_or_default()
    }
}

/// Read one render endpoint's id, friendly name and volume/mute into a
/// `SinkVolume`. `None` when the endpoint has no usable id (can't be keyed).
unsafe fn device_to_sink(dev: &IMMDevice) -> Option<SinkVolume> {
    unsafe {
        let name = endpoint_id(dev);
        if name.is_empty() {
            return None;
        }
        let description = endpoint_friendly_name(dev);
        // Activate the per-endpoint volume interface. Best-effort: an endpoint
        // that refuses activation is still listed (at its last-known 0/false).
        let (volume, muted) = match dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
            Ok(vol) => {
                let v = f64::from(vol.GetMasterVolumeLevelScalar().unwrap_or(0.0));
                // `as_bool` takes `&self` but `is_ok_and` yields `BOOL` by value,
                // so the bare method reference clippy prefers doesn't type-check.
                #[allow(clippy::redundant_closure_for_method_calls)]
                let m = vol.GetMute().is_ok_and(|b| b.as_bool());
                (v.clamp(0.0, 1.0), m)
            }
            Err(_) => (0.0, false),
        };
        Some(SinkVolume {
            name,
            description,
            volume,
            muted,
        })
    }
}

/// Enumerate active render endpoints as `SinkVolume`s (id, friendly name, live
/// perceptual volume + mute). Returns an empty vec if COM/enumeration fails.
#[must_use]
pub fn enumerate() -> Vec<SinkVolume> {
    with_render_endpoints(|enumerator| {
        let coll = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
        let count = unsafe { coll.GetCount()? };
        let mut sinks = Vec::with_capacity(count as usize);
        for i in 0..count {
            if let Ok(dev) = unsafe { coll.Item(i) }
                && let Some(sink) = unsafe { device_to_sink(&dev) }
            {
                sinks.push(sink);
            }
        }
        Ok(sinks)
    })
    .unwrap_or_default()
}

/// Find the render endpoint whose id equals `name` and apply `f` to its
/// `IAudioEndpointVolume`. Returns whether a match was found and applied.
fn apply_to_endpoint(
    name: &str,
    f: impl Fn(&IAudioEndpointVolume) -> windows::core::Result<()>,
) -> bool {
    with_render_endpoints(|enumerator| {
        let coll = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
        let count = unsafe { coll.GetCount()? };
        for i in 0..count {
            let matched = unsafe {
                let dev = coll.Item(i)?;
                if endpoint_id(&dev) == name {
                    let vol: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
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

/// Set an endpoint's master volume. The endpoint scalar is perceptual 0..1
/// (matches the Windows slider), so `volume` is written directly — NO cube —
/// clamped to 0..=1 (the scalar caps at unity; Windows has no endpoint boost).
pub fn set_volume(name: &str, volume: f64) -> bool {
    let level = volume.clamp(0.0, 1.0) as f32;
    apply_to_endpoint(name, |vol| unsafe {
        vol.SetMasterVolumeLevelScalar(level, std::ptr::null())
    })
}

/// Mute or unmute an endpoint.
pub fn set_mute(name: &str, muted: bool) -> bool {
    apply_to_endpoint(name, |vol| unsafe { vol.SetMute(muted, std::ptr::null()) })
}

/// Spawn the Windows per-sink control plane: a thread that polls the render
/// endpoints and publishes their volume/mute, and a thread that applies incoming
/// requests. Both own COM (via `with_render_endpoints`); they run for the
/// daemon's lifetime. Mirrors [`super::win_apps::spawn_app_tasks`].
///
/// The ~500ms enumeration cadence catches external volume changes (Windows
/// volume mixer, media keys) for two-way sync — the analog of the Linux
/// backend's ~200ms `poll_routes`.
pub fn spawn_sink_tasks(
    sinks_vol_tx: tokio::sync::mpsc::UnboundedSender<Vec<SinkVolume>>,
    sink_ctl_rx: std::sync::mpsc::Receiver<SinkCtl>,
) {
    std::thread::Builder::new()
        .name("resonance-win-sinks".into())
        .spawn(move || {
            loop {
                if sinks_vol_tx.send(enumerate()).is_err() {
                    break; // daemon shutting down
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })
        .ok();

    std::thread::Builder::new()
        .name("resonance-win-sinkctl".into())
        .spawn(move || {
            while let Ok(ctl) = sink_ctl_rx.recv() {
                match ctl {
                    SinkCtl::SetVolume { name, volume } => {
                        let _ = set_volume(&name, volume);
                    }
                    SinkCtl::SetMute { name, muted } => {
                        let _ = set_mute(&name, muted);
                    }
                }
            }
        })
        .ok();
}
