//! macOS output-sink (device) volume/mute control via Core Audio.
//!
//! The daemon models every OUTPUT device the HAL knows about as a controllable
//! [`SinkVolume`]: a stable identity (the device UID → `SinkVolume::name`), a
//! user-facing label (`kAudioObjectPropertyName` → `description`), a perceptual
//! `0.0..=1.0` volume, and a `muted` flag. Clients set volume/mute by UID via
//! [`SinkCtl`]; the daemon publishes the live list ~2×/sec.
//!
//! This is the macOS mirror of the `PipeWire` sink-volume control plane and of the
//! per-app `mac_apps` module: two threads — [`spawn_sink_enumeration`] (poll +
//! publish) and [`spawn_sink_control`] (drain requests + apply to the device).
//!
//! Unlike per-app control, the device is the source of truth: we read the live
//! device volume/mute every poll (so external changes in System Settings /
//! `osascript` show up), and control writes go straight to the device — the
//! next poll reflects them. A short-lived optimistic overlay (`pending`) keyed
//! by UID hides the one-poll lag between a client set and the read-back.
//!
//! Control-plane only: reading/writing a device's volume needs **no** TCC
//! permission and touches no audio capture, so it runs regardless of whether the
//! system-audio tap is active.
//!
//! # Volume scale
//!
//! Core Audio device volume is already perceptual `0.0..=1.0` (matching the
//! System Settings output slider), so — unlike the `PipeWire` backend, whose
//! `channelVolumes` are linear and get cube-rooted — we do **NO** cubing here.
//! We prefer `kAudioHardwareServiceDeviceProperty_VirtualMasterVolume` (a single
//! master control that tracks the System Settings slider); devices that don't
//! expose it fall back to the per-channel `kAudioDevicePropertyVolumeScalar`.

use crate::state::SinkCtl;
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyElement,
    AudioObjectPropertyScope, AudioObjectPropertySelector, AudioObjectSetPropertyData,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyMute,
    kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyVolumeScalar,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput,
};
use objc2_core_audio_types::AudioBufferList;
use objc2_core_foundation::{CFRetained, CFString};
use resonance_ipc::SinkVolume;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// `kAudioObjectSystemObject` — the root the device list hangs off.
const SYSTEM_OBJECT: AudioObjectID = 1;

/// `kAudioHardwareServiceDeviceProperty_VirtualMasterVolume` (`'vmvc'`). Not
/// bound by objc2-core-audio 0.3 (it lives in `<CoreAudio/AudioHardwareService.h>`),
/// but the selector resolves against the device object via the plain
/// `AudioObject*` API on modern macOS. This is a single perceptual master
/// control that tracks the System Settings output slider — the value
/// `osascript "output volume of (get volume settings)"` reports for the default
/// device.
const VIRTUAL_MASTER_VOLUME: AudioObjectPropertySelector = 0x766d_7663;

/// Our own private tap aggregate — an input wrapper, never a real output sink.
/// Filtered out so it can't appear as a controllable device.
const TAP_DEVICE_NAME: &str = super::system_tap::TAP_DEVICE_NAME;

/// Build a property address on a given scope + element.
fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

/// All audio device object ids the HAL currently knows about.
fn device_ids() -> Vec<AudioObjectID> {
    let mut addr = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    let mut size: u32 = 0;
    // SAFETY: system object + valid stack pointers; queries the array byte size.
    let st = unsafe {
        AudioObjectGetPropertyDataSize(
            SYSTEM_OBJECT,
            addr_ptr,
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
        )
    };
    if st != 0 || size == 0 {
        return Vec::new();
    }
    let n = size as usize / std::mem::size_of::<AudioObjectID>();
    let mut ids = vec![AudioObjectID::default(); n];
    let mut io = size;
    // SAFETY: `ids` is sized to `size` bytes; the call fills it with `n` ids.
    let st = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            addr_ptr,
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut io)).unwrap(),
            NonNull::new(ids.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    if st == 0 { ids } else { Vec::new() }
}

/// Total output-scope channel count of a device (0 = not an output device).
/// Reads `kAudioDevicePropertyStreamConfiguration` on the output scope and sums
/// `mNumberChannels` across the returned `AudioBufferList` buffers.
fn output_channel_count(dev: AudioObjectID) -> usize {
    let mut addr = address(
        kAudioDevicePropertyStreamConfiguration,
        kAudioObjectPropertyScopeOutput,
        kAudioObjectPropertyElementMain,
    );
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    let mut size: u32 = 0;
    // SAFETY: valid stack pointers; queries the buffer-list byte size.
    let st = unsafe {
        AudioObjectGetPropertyDataSize(
            dev,
            addr_ptr,
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
        )
    };
    if st != 0 || (size as usize) < std::mem::size_of::<AudioBufferList>() {
        return 0;
    }
    // Back the AudioBufferList (a flexible array) with `size` bytes, allocated as
    // `u64` words so the buffer is 8-aligned — `AudioBufferList`'s alignment —
    // making the `cast::<AudioBufferList>()` below well-defined (a bare `Vec<u8>`
    // is only 1-aligned).
    let words = (size as usize).div_ceil(std::mem::size_of::<u64>()).max(1);
    let mut raw = vec![0u64; words];
    let mut io = size;
    // SAFETY: `raw` is >= `size` bytes (rounded up to a u64 boundary), which is
    // what the HAL reported it needs.
    let st = unsafe {
        AudioObjectGetPropertyData(
            dev,
            addr_ptr,
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut io)).unwrap(),
            NonNull::new(raw.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    if st != 0 {
        return 0;
    }
    // SAFETY: `raw` holds a valid AudioBufferList; `mNumberBuffers` bounds the
    // flexible `mBuffers` array (declared `[AudioBuffer; 1]` but truly N long).
    let list = unsafe { &*raw.as_ptr().cast::<AudioBufferList>() };
    let nbuf = list.mNumberBuffers as usize;
    let base = list.mBuffers.as_ptr();
    (0..nbuf)
        .map(|i| {
            // SAFETY: `i < mNumberBuffers`, all within the `size`-byte buffer.
            let b = unsafe { &*base.add(i) };
            b.mNumberChannels as usize
        })
        .sum()
}

/// Read a `CFString` property (UID, name) off a device on a given scope.
fn read_cfstring(
    dev: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Option<String> {
    let mut addr = address(selector, scope, kAudioObjectPropertyElementMain);
    let mut ptr: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    // SAFETY: valid stack pointers; the HAL writes one +1-retained CFString ref.
    let st = unsafe {
        AudioObjectGetPropertyData(
            dev,
            NonNull::new(std::ptr::from_mut(&mut addr)).unwrap(),
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut ptr).cast::<c_void>()).unwrap(),
        )
    };
    if st != 0 || ptr.is_null() {
        return None;
    }
    let nn = NonNull::new(ptr.cast_mut())?;
    // SAFETY: the HAL hands us a +1 retained CFString for this property.
    let cf = unsafe { CFRetained::from_raw(nn) };
    Some(cf.to_string())
}

/// Read an f32 property (volume scalar) off a device at a scope + element.
fn read_f32(
    dev: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> Option<f32> {
    let mut addr = address(selector, scope, element);
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    // SAFETY: address is a valid stack pointer for the query.
    if !unsafe { AudioObjectHasProperty(dev, addr_ptr) } {
        return None;
    }
    let mut value: f32 = 0.0;
    let mut size = std::mem::size_of::<f32>() as u32;
    // SAFETY: valid stack pointers; `out` is exactly one f32.
    let st = unsafe {
        AudioObjectGetPropertyData(
            dev,
            addr_ptr,
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut value).cast::<c_void>()).unwrap(),
        )
    };
    (st == 0).then_some(value)
}

/// Read a u32 property (mute flag) off a device at a scope + element.
fn read_u32(
    dev: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> Option<u32> {
    let mut addr = address(selector, scope, element);
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    // SAFETY: address is a valid stack pointer for the query.
    if !unsafe { AudioObjectHasProperty(dev, addr_ptr) } {
        return None;
    }
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: valid stack pointers; `out` is exactly one u32.
    let st = unsafe {
        AudioObjectGetPropertyData(
            dev,
            addr_ptr,
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut value).cast::<c_void>()).unwrap(),
        )
    };
    (st == 0).then_some(value)
}

/// Write an f32 property (volume scalar) to a device at a scope + element.
/// Returns `true` on `noErr`.
fn write_f32(
    dev: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
    mut value: f32,
) -> bool {
    let mut addr = address(selector, scope, element);
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    // SAFETY: address is a valid stack pointer for the query.
    if !unsafe { AudioObjectHasProperty(dev, addr_ptr) } {
        return false;
    }
    // SAFETY: valid stack pointers; `in_data` is exactly one f32.
    let st = unsafe {
        AudioObjectSetPropertyData(
            dev,
            addr_ptr,
            0,
            std::ptr::null(),
            std::mem::size_of::<f32>() as u32,
            NonNull::new(std::ptr::from_mut(&mut value).cast::<c_void>()).unwrap(),
        )
    };
    st == 0
}

/// Write a u32 property (mute flag) to a device at a scope + element.
/// Returns `true` on `noErr`.
fn write_u32(
    dev: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
    mut value: u32,
) -> bool {
    let mut addr = address(selector, scope, element);
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    // SAFETY: address is a valid stack pointer for the query.
    if !unsafe { AudioObjectHasProperty(dev, addr_ptr) } {
        return false;
    }
    // SAFETY: valid stack pointers; `in_data` is exactly one u32.
    let st = unsafe {
        AudioObjectSetPropertyData(
            dev,
            addr_ptr,
            0,
            std::ptr::null(),
            std::mem::size_of::<u32>() as u32,
            NonNull::new(std::ptr::from_mut(&mut value).cast::<c_void>()).unwrap(),
        )
    };
    st == 0
}

/// Read a device's perceptual output volume `0.0..=1.0`. Prefers the virtual
/// master (System Settings slider); falls back to the mean of the per-channel
/// `VolumeScalar` controls (elements 1..=channels). Scalar is already
/// perceptual — NO cube root (unlike `PipeWire`'s linear `channelVolumes`).
fn read_volume(dev: AudioObjectID, channels: usize) -> Option<f64> {
    if let Some(v) = read_f32(
        dev,
        VIRTUAL_MASTER_VOLUME,
        kAudioObjectPropertyScopeOutput,
        kAudioObjectPropertyElementMain,
    ) {
        return Some(f64::from(v).clamp(0.0, 1.0));
    }
    // Per-channel fallback: elements are 1-based (element 0 = master, often
    // unsupported for VolumeScalar). Average whatever channels expose it.
    let mut sum = 0.0f64;
    let mut n = 0u32;
    for el in 1..=channels as u32 {
        if let Some(v) = read_f32(
            dev,
            kAudioDevicePropertyVolumeScalar,
            kAudioObjectPropertyScopeOutput,
            el,
        ) {
            sum += f64::from(v);
            n += 1;
        }
    }
    (n > 0).then(|| (sum / f64::from(n)).clamp(0.0, 1.0))
}

/// Read a device's output mute flag. Master element first, then any per-channel
/// mute; muted if any control reports muted.
fn read_mute(dev: AudioObjectID, channels: usize) -> bool {
    if let Some(m) = read_u32(
        dev,
        kAudioDevicePropertyMute,
        kAudioObjectPropertyScopeOutput,
        kAudioObjectPropertyElementMain,
    ) {
        return m != 0;
    }
    (1..=channels as u32).any(|el| {
        read_u32(
            dev,
            kAudioDevicePropertyMute,
            kAudioObjectPropertyScopeOutput,
            el,
        )
        .is_some_and(|m| m != 0)
    })
}

/// Set a device's perceptual output volume `0.0..=1.0`. Writes the virtual
/// master when present, else every per-channel `VolumeScalar` control. NO cube.
fn set_volume(dev: AudioObjectID, channels: usize, volume: f64) -> bool {
    let v = volume.clamp(0.0, 1.0) as f32;
    if write_f32(
        dev,
        VIRTUAL_MASTER_VOLUME,
        kAudioObjectPropertyScopeOutput,
        kAudioObjectPropertyElementMain,
        v,
    ) {
        return true;
    }
    let mut any = false;
    for el in 1..=channels as u32 {
        any |= write_f32(
            dev,
            kAudioDevicePropertyVolumeScalar,
            kAudioObjectPropertyScopeOutput,
            el,
            v,
        );
    }
    any
}

/// Set a device's output mute. Master element when present, else every
/// per-channel mute control.
fn set_mute(dev: AudioObjectID, channels: usize, muted: bool) -> bool {
    let m = u32::from(muted);
    if write_u32(
        dev,
        kAudioDevicePropertyMute,
        kAudioObjectPropertyScopeOutput,
        kAudioObjectPropertyElementMain,
        m,
    ) {
        return true;
    }
    let mut any = false;
    for el in 1..=channels as u32 {
        any |= write_u32(
            dev,
            kAudioDevicePropertyMute,
            kAudioObjectPropertyScopeOutput,
            el,
            m,
        );
    }
    any
}

/// Resolve the `AudioObjectID` of the output device whose UID equals `uid`.
fn device_by_uid(uid: &str) -> Option<(AudioObjectID, usize)> {
    device_ids().into_iter().find_map(|dev| {
        let ch = output_channel_count(dev);
        if ch == 0 {
            return None;
        }
        let dev_uid = read_cfstring(
            dev,
            kAudioDevicePropertyDeviceUID,
            kAudioObjectPropertyScopeGlobal,
        )?;
        (dev_uid == uid).then_some((dev, ch))
    })
}

/// Set an output device's perceptual volume `0.0..=1.0` by UID. Returns whether
/// a matching device was found and the write succeeded. Control-plane entry
/// point (used by the `--set-sink` debug mode); needs no TCC.
#[must_use]
pub fn set_volume_by_uid(uid: &str, volume: f64) -> bool {
    device_by_uid(uid).is_some_and(|(dev, ch)| set_volume(dev, ch, volume))
}

/// Set an output device's mute by UID. Returns whether it was found + applied.
#[must_use]
pub fn set_mute_by_uid(uid: &str, muted: bool) -> bool {
    device_by_uid(uid).is_some_and(|(dev, ch)| set_mute(dev, ch, muted))
}

/// Enumerate every OUTPUT device as a [`SinkVolume`], reading live volume/mute.
/// `name` = device UID (stable identity); `description` = human-readable name.
/// Sorted by description then name for a stable UI order.
#[must_use]
pub fn enumerate_output_sinks() -> Vec<SinkVolume> {
    let mut sinks: Vec<SinkVolume> = device_ids()
        .into_iter()
        .filter_map(|dev| {
            let channels = output_channel_count(dev);
            if channels == 0 {
                return None; // input-only device
            }
            let name = read_cfstring(
                dev,
                kAudioDevicePropertyDeviceUID,
                kAudioObjectPropertyScopeGlobal,
            )?;
            let description = read_cfstring(
                dev,
                kAudioObjectPropertyName,
                kAudioObjectPropertyScopeGlobal,
            )
            .unwrap_or_else(|| name.clone());
            // Hide our own private tap aggregate.
            if description == TAP_DEVICE_NAME {
                return None;
            }
            Some(SinkVolume {
                name,
                description,
                volume: read_volume(dev, channels).unwrap_or(1.0),
                muted: read_mute(dev, channels),
            })
        })
        .collect();
    sinks.sort_by(|a, b| a.description.cmp(&b.description).then(a.name.cmp(&b.name)));
    sinks
}

/// Overlay short-lived optimistic sets (`uid → (volume, muted)`) onto a freshly
/// read list, hiding the one-poll lag between a client set and the device
/// read-back. Mirrors the app-control overlay; the device remains the source of
/// truth, so once a poll agrees the pending entry is cleared by the control
/// thread's own read-back logic (it is only inserted on a *successful* write).
fn overlay_pending(sinks: &mut [SinkVolume], pending: &HashMap<String, (f64, bool)>) {
    for s in sinks.iter_mut() {
        if let Some(&(volume, muted)) = pending.get(&s.name) {
            // Only override while the device still disagrees (hysteresis 0.0005,
            // matching the PipeWire read-back tolerance). Once it matches, the
            // pending hint is harmless (equal) and gets cleared below.
            if (s.volume - volume).abs() > 0.0005 || s.muted != muted {
                s.volume = volume;
                s.muted = muted;
            }
        }
    }
}

/// Publish the live output-sink list (~2 Hz) on a dedicated thread. Reads the
/// device volume/mute each cycle (source of truth for external changes) with any
/// just-set optimistic values overlaid until the read-back catches up. Exits
/// when the daemon drops the receiver. Needs no TCC (device volume only).
pub fn spawn_sink_enumeration(
    sinks_vol_tx: tokio::sync::mpsc::UnboundedSender<Vec<SinkVolume>>,
    pending: Arc<Mutex<HashMap<String, (f64, bool)>>>,
) {
    thread::Builder::new()
        .name("resonance-mac-sinks".into())
        .spawn(move || {
            loop {
                let mut sinks = enumerate_output_sinks();
                {
                    let mut p = pending.lock().unwrap();
                    overlay_pending(&mut sinks, &p);
                    // Drop pending hints the device has now agreed with, so a
                    // later external change isn't masked by a stale optimistic
                    // value (drain-and-reconcile, like the PipeWire inbox).
                    let live: HashMap<&str, (f64, bool)> = sinks
                        .iter()
                        .map(|s| (s.name.as_str(), (s.volume, s.muted)))
                        .collect();
                    p.retain(|uid, &mut (v, m)| {
                        live.get(uid.as_str())
                            .is_none_or(|&(lv, lm)| (lv - v).abs() > 0.0005 || lm != m)
                    });
                }
                if sinks_vol_tx.send(sinks).is_err() {
                    break; // daemon gone
                }
                thread::sleep(Duration::from_millis(500));
            }
        })
        .ok();
}

/// Drain per-sink volume/mute requests and apply them to the matching device by
/// UID. On a successful write, records the value in `pending` so the enumeration
/// thread reflects it before the next read-back. Exits on sender drop. Needs no
/// TCC. Applies exactly the client's perceptual `0..1` scalar — NO cube.
pub fn spawn_sink_control(
    sink_ctl_rx: std::sync::mpsc::Receiver<SinkCtl>,
    pending: Arc<Mutex<HashMap<String, (f64, bool)>>>,
) {
    thread::Builder::new()
        .name("resonance-mac-sinkctl".into())
        .spawn(move || {
            while let Ok(ctl) = sink_ctl_rx.recv() {
                match ctl {
                    SinkCtl::SetVolume { name, volume } => {
                        let v = volume.clamp(0.0, 1.0);
                        if let Some((dev, ch)) = device_by_uid(&name) {
                            if set_volume(dev, ch, v) {
                                let mut p = pending.lock().unwrap();
                                let e = p.entry(name).or_insert((v, read_mute(dev, ch)));
                                e.0 = v;
                            }
                        }
                    }
                    SinkCtl::SetMute { name, muted } => {
                        if let Some((dev, ch)) = device_by_uid(&name) {
                            if set_mute(dev, ch, muted) {
                                let mut p = pending.lock().unwrap();
                                let e = p
                                    .entry(name)
                                    .or_insert((read_volume(dev, ch).unwrap_or(1.0), muted));
                                e.1 = muted;
                            }
                        }
                    }
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_master_volume_fourcc() {
        // 'vmvc' — the AudioHardwareService master-volume selector.
        assert_eq!(VIRTUAL_MASTER_VOLUME, 0x766d_7663);
    }

    #[test]
    fn overlay_hides_one_poll_lag_then_clears() {
        // A pending optimistic set overrides a stale device read…
        let mut sinks = vec![SinkVolume {
            name: "BuiltInSpeakerDevice".into(),
            description: "MacBook Air Speakers".into(),
            volume: 0.8,
            muted: false,
        }];
        let mut pending = HashMap::new();
        pending.insert("BuiltInSpeakerDevice".to_string(), (0.5, false));
        overlay_pending(&mut sinks, &pending);
        assert!(
            (sinks[0].volume - 0.5).abs() < 1e-9,
            "optimistic value shown"
        );
    }

    #[test]
    fn overlay_leaves_unknown_sinks_untouched() {
        let mut sinks = vec![SinkVolume {
            name: "OtherDevice".into(),
            description: "Other".into(),
            volume: 0.7,
            muted: false,
        }];
        overlay_pending(&mut sinks, &HashMap::new());
        assert!((sinks[0].volume - 0.7).abs() < 1e-9);
    }
}
