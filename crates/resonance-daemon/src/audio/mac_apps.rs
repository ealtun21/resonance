//! macOS per-application audio enumeration via Core Audio process objects.
//!
//! Lists the processes the audio HAL knows about (`kAudioHardwarePropertyProcessObjectList`),
//! reads each one's bundle id / pid / output-running state, and maps the ones
//! currently producing output into [`AppStream`]s for the daemon's per-app list.
//!
//! This is the read-only foundation of the macOS per-app mixer (increment 9a):
//! it touches no taps and needs no TCC permission. Per-app volume control (the
//! muted-tap mixer) builds on this list.

use super::app_streams::friendly_name_from_bundle;
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertySelector,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID,
};
use objc2_core_foundation::{CFRetained, CFString};
use resonance_ipc::AppStream;
use std::ffi::c_void;
use std::ptr::NonNull;

/// `kAudioObjectSystemObject` — the root object the process list hangs off.
const SYSTEM_OBJECT: AudioObjectID = 1;
/// Our own daemon/GUI bundle id — never list ourselves as a controllable app.
const OWN_BUNDLE: &str = "com.ealtun21.resonance";

fn global_address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// The audio HAL's current list of process objects.
fn process_object_list() -> Vec<AudioObjectID> {
    let mut addr = global_address(kAudioHardwarePropertyProcessObjectList);
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

/// Read a fixed-size scalar (`i32` pid, `u32` flag) property off a process object.
fn read_scalar<T: Copy + Default>(
    obj: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Option<T> {
    let mut addr = global_address(selector);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    // SAFETY: valid stack pointers; `out` is exactly `size` bytes for `T`.
    let st = unsafe {
        AudioObjectGetPropertyData(
            obj,
            NonNull::new(std::ptr::from_mut(&mut addr)).unwrap(),
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut value).cast::<c_void>()).unwrap(),
        )
    };
    (st == 0).then_some(value)
}

/// Read a `CFString` property (e.g. the bundle id) off a process object.
fn read_cfstring(obj: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<String> {
    let mut addr = global_address(selector);
    let mut ptr: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    // SAFETY: valid stack pointers; the HAL writes one +1-retained CFString ref.
    let st = unsafe {
        AudioObjectGetPropertyData(
            obj,
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

/// `(key, process_object_id)` for each application currently producing output
/// audio, skipping processes with no bundle id and our own daemon. The `key` is
/// the bundle id (stable across runs); the process object id is what the per-app
/// mixer taps. (9a: running-output only; idle apps are a later refinement.)
#[must_use]
pub fn enumerate_targets() -> Vec<(String, u32)> {
    process_object_list()
        .into_iter()
        .filter_map(|obj| {
            let bundle = read_cfstring(obj, kAudioProcessPropertyBundleID)?;
            if bundle.is_empty() || bundle == OWN_BUNDLE {
                return None;
            }
            let running = read_scalar::<u32>(obj, kAudioProcessPropertyIsRunningOutput)? != 0;
            running.then_some((bundle, obj))
        })
        .collect()
}

/// Enumerate applications currently producing output audio as [`AppStream`]s.
/// Sorted by display name for a stable UI order.
#[must_use]
pub fn enumerate() -> Vec<AppStream> {
    let mut apps: Vec<AppStream> = enumerate_targets()
        .into_iter()
        .map(|(bundle, obj)| {
            let pid = read_scalar::<i32>(obj, kAudioProcessPropertyPID).unwrap_or(-1);
            AppStream {
                display_name: friendly_name_from_bundle(&bundle),
                key: bundle,
                pid: u32::try_from(pid).ok(),
                volume: 1.0,
                muted: false,
                active: true,
            }
        })
        .collect();
    apps.sort_by(|a, b| a.display_name.cmp(&b.display_name).then(a.key.cmp(&b.key)));
    apps
}
