//! Native macOS system-audio capture via Core Audio's Process Tap API.
//!
//! Apple introduced [`CATapDescription`] + [`AudioHardwareCreateProcessTap`]
//! in macOS 14.2. They let an unprivileged user-space process *tap* the
//! audio that other processes send to the system mixer, without installing
//! a kernel extension or an `AudioServerPlugIn` (which is what BlackHole or
//! Loopback ship). Apple Music, browsers, games, calls — everything that
//! plays through the system goes through the tap.
//!
//! Architecture this module sets up:
//!
//!   1. `CATapDescription` — stereo mixdown of every running process
//!      (global tap with an empty exclude list), `private = true` so other
//!      audio clients can't see it, `muteBehavior = MutedWhenTapped` so the
//!      system stops sending the original audio to the speakers while the
//!      tap is open (we re-render it from the DSP path instead).
//!   2. `AudioHardwareCreateProcessTap` → an `AudioObjectID` that
//!      represents the tap as a Core Audio object with a UID.
//!   3. `AudioHardwareCreateAggregateDevice` — a private aggregate device
//!      that wraps the tap as its sole sub-tap. Aggregate devices appear in
//!      the system audio HAL as ordinary input devices, so cpal can open
//!      this one via its normal input-stream path.
//!
//! On drop the aggregate device and tap are destroyed, restoring normal
//! routing so the system isn't left in a half-tapped state if the daemon
//! crashes or exits.
//!
//! # Permission
//!
//! macOS gates Process Tap behind the "System Audio Capture" privacy
//! permission (Privacy & Security → Microphone on 14.x, → Screen & System
//! Audio Recording on 15+). The first call to `AudioHardwareCreateProcessTap`
//! triggers the TCC prompt; if the user denies it the call fails with a
//! non-zero `OSStatus` and we surface a clear error message.

use anyhow::{Result, anyhow};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyUID,
};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFMutableDictionary, CFRetained, CFString, CFType, kCFBooleanFalse,
    kCFBooleanTrue,
};
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use tracing::{info, warn};

/// Name the aggregate device shows up as in cpal/Audio MIDI Setup. cpal
/// enumerates devices by name; we look it up by exactly this string.
pub const TAP_DEVICE_NAME: &str = "Resonance EQ Tap";
/// Stable UID for the aggregate device. UID is what `AudioMIDI` / cpal use
/// to identify devices across reboots; keep ours unique to the app.
const AGGREGATE_DEVICE_UID: &str = "com.ealtun21.resonance.tap-aggregate";

/// Owns the Core Audio objects backing the system tap. Drops them on scope
/// exit so the system goes back to normal routing even on panic.
pub struct SystemAudioTap {
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
}

impl SystemAudioTap {
    /// Create the tap + aggregate device. Returns Err if we lack permission,
    /// if the API is unavailable (pre-14.2 macOS), or if Core Audio rejects
    /// our description.
    pub fn create() -> Result<Self> {
        // macOS gates Process Tap behind kTCCServiceAudioCapture. Apple
        // doesn't expose a public API to prompt for it — without an
        // explicit prompt the system silently returns zero-filled buffers.
        // Insidegui's AudioCap sample (the canonical working reference)
        // uses the private TCC framework to trigger the prompt. We do the
        // same: preflight to skip if already decided, otherwise request.
        request_audio_capture_permission();
        // Exclude OURSELVES from the tap. The tap captures the audio of
        // every process listed in `kAudioAggregateDeviceTapListKey`, AND
        // our daemon writes the DSP-processed audio back to the speakers
        // via a cpal output stream. If we don't exclude resonanced, that
        // output gets tapped → DSP again → written out → tapped → …
        // hard feedback loop, audible as runaway noise.
        //
        // The exclude list takes process AudioObjectIDs, not POSIX PIDs,
        // so we translate our own pid via TranslatePIDToProcessObject.
        let mut excludes: Vec<Retained<NSNumber>> = Vec::new();
        let self_pid = unsafe { libc::getpid() } as i32;
        if let Some(self_obj) = translate_pid_to_process_object(self_pid) {
            excludes.push(NSNumber::new_u32(self_obj));
            info!(
                "excluding self (pid={self_pid}, process_obj={self_obj}) from tap to prevent feedback loop"
            );
        } else {
            warn!(
                "could not translate self pid {self_pid} to AudioObjectID — \
                 feedback loop possible (tap will capture our own output)"
            );
        }
        let exclude_refs: Vec<&NSNumber> = excludes.iter().map(Retained::as_ref).collect();
        let exclude_arr: Retained<NSArray<NSNumber>> = NSArray::from_slice(&exclude_refs);
        // SAFETY: CATapDescription's allocator/init pair are a normal
        // Objective-C alloc/init sequence. With Exclusive=YES (set below)
        // and the exclude list containing our own process, the tap covers
        // every process EXCEPT us.
        let desc: Retained<CATapDescription> = unsafe {
            CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &exclude_arr,
            )
        };
        unsafe {
            desc.setName(&NSString::from_str(TAP_DEVICE_NAME));
            // private = true: the tap is invisible to other Core Audio
            // clients (e.g. Audio MIDI Setup) so it doesn't pollute the UI.
            desc.setPrivate(true);
            // Muted: audio is captured by the tap and NOT routed to
            // hardware. We replay the DSP-processed version via the
            // cpal output stream so the user hears only the processed
            // signal. Unmuted caused audible doubling (original + DSP
            // both reaching the speakers).
            desc.setMuteBehavior(CATapMuteBehavior::Muted);
            // CRITICAL: Exclusive=YES means "tap every process EXCEPT
            // those in the list". With an empty list this means "tap all
            // processes". Exclusive=NO with empty list = "tap nothing"
            // (silent buffers — the bug we hit). sudara's known-working
            // CoreAudio Tap example uses Exclusive=YES with empty list
            // for a global tap.
            desc.setExclusive(true);
        }

        let mut tap_id: AudioObjectID = 0;
        // SAFETY: tap_id is a stack u32, valid for write. desc lives for
        // the duration of this call. A non-zero status means the system
        // rejected the request (most commonly: missing TCC permission).
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id) };
        if status != 0 || tap_id == 0 {
            return Err(anyhow!(
                "AudioHardwareCreateProcessTap failed (status={status}) — \
                 grant System Audio Capture permission in System Settings \
                 → Privacy & Security → Microphone, then restart resonanced"
            ));
        }

        let tap_uid_string = match get_tap_uid(tap_id) {
            Ok(uid) => uid,
            Err(e) => {
                // SAFETY: tap_id was created above and not yet returned.
                unsafe {
                    let _ = AudioHardwareDestroyProcessTap(tap_id);
                }
                return Err(e);
            }
        };

        // Apple's WWDC23 CapturingSystemAudio sample does NOT set a
        // main_sub_device when the aggregate's only purpose is a tap — the
        // tap provides its own clock from the original audio source. When
        // we passed the speakers' UID as the main device, the aggregate
        // treated the speakers as the primary stream and the tap as a
        // secondary input that never received samples. Leaving main_sub
        // unset lets the tap be the source.
        let dict = build_aggregate_dict(&tap_uid_string, None);

        let mut agg_id: AudioObjectID = 0;
        let agg_ptr = NonNull::new(std::ptr::from_mut(&mut agg_id)).unwrap();
        // SAFETY: dict is a valid CFDictionary built above with the right
        // keys/value types per Apple's aggregate-device specification.
        // agg_ptr is a valid pointer to a stack u32. CFMutableDictionary
        // upcasts to CFDictionary at the FFI boundary (same opaque header).
        let status = unsafe {
            let dict_ptr = CFRetained::as_ptr(&dict).cast::<CFDictionary>();
            AudioHardwareCreateAggregateDevice(dict_ptr.as_ref(), agg_ptr)
        };
        if status != 0 || agg_id == 0 {
            unsafe {
                let _ = AudioHardwareDestroyProcessTap(tap_id);
            }
            return Err(anyhow!(
                "AudioHardwareCreateAggregateDevice failed (status={status})"
            ));
        }

        info!(
            "Process tap ready (tap_id={tap_id}, aggregate_id={agg_id}, \
             aggregate_name='{TAP_DEVICE_NAME}')"
        );
        Ok(Self {
            tap_id,
            aggregate_id: agg_id,
        })
    }

    /// Aggregate device name — what cpal sees when enumerating inputs.
    #[allow(dead_code)] // intended for debug logging from callers
    pub fn device_name() -> &'static str {
        TAP_DEVICE_NAME
    }

    /// Raw `AudioObjectID` of the private aggregate device. The raw HAL
    /// input path (`hal_input.rs`) registers an `AudioDeviceIOProcID` on
    /// this id to read tap samples directly — bypassing cpal/AUHAL, which
    /// doesn't reliably expose the tap stream on aggregate devices.
    pub fn aggregate_id(&self) -> AudioObjectID {
        self.aggregate_id
    }
}

impl Drop for SystemAudioTap {
    fn drop(&mut self) {
        // SAFETY: ids were assigned by successful HAL calls; destroy is
        // idempotent / safe on already-gone objects (returns an error).
        unsafe {
            if self.aggregate_id != 0 {
                let status = AudioHardwareDestroyAggregateDevice(self.aggregate_id);
                if status != 0 {
                    warn!(
                        "AudioHardwareDestroyAggregateDevice failed (id={}, status={status})",
                        self.aggregate_id
                    );
                }
            }
            if self.tap_id != 0 {
                let status = AudioHardwareDestroyProcessTap(self.tap_id);
                if status != 0 {
                    warn!(
                        "AudioHardwareDestroyProcessTap failed (id={}, status={status})",
                        self.tap_id
                    );
                }
            }
        }
    }
}

// SAFETY: AudioObjectIDs are plain u32s and the destroy syscalls are
// thread-safe (HAL holds its own internal locks). The struct holds no
// thread-affine state.
unsafe impl Send for SystemAudioTap {}
unsafe impl Sync for SystemAudioTap {}

/// Query the tap's UID. `AudioObjectGetPropertyData` returns a `CFStringRef`
/// (= `*const CFString`) when the selector is `kAudioTapPropertyUID`.
fn get_tap_uid(tap_id: AudioObjectID) -> Result<CFRetained<CFString>> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let addr_ptr = NonNull::new(&mut addr).unwrap();
    let mut data_size: u32 = std::mem::size_of::<*const CFString>() as u32;
    let size_ptr = NonNull::new(&mut data_size).unwrap();
    let mut uid_ptr: *const CFString = std::ptr::null();
    // SAFETY: addr/size/uid_ptr are all valid stack pointers; HAL writes
    // a CFString pointer into uid_ptr on success. The property API returns
    // a +1 reference for CFString properties — we wrap it in CFRetained so
    // the release happens on drop.
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap_id,
            addr_ptr,
            0,
            std::ptr::null(),
            size_ptr,
            NonNull::new(std::ptr::from_mut(&mut uid_ptr).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 || uid_ptr.is_null() {
        return Err(anyhow!(
            "AudioObjectGetPropertyData(kAudioTapPropertyUID) failed (status={status})"
        ));
    }
    let uid_nn = NonNull::new(uid_ptr.cast_mut())
        .ok_or_else(|| anyhow!("tap UID property returned null"))?;
    // SAFETY: HAL handed us a +1 retained CFString; from_raw transfers
    // that ownership into the CFRetained wrapper.
    let retained = unsafe { CFRetained::from_raw(uid_nn) };
    Ok(retained)
}

/// Convert a static C-string aggregate-device key into a `CFString`. Apple's
/// key constants come through objc2-core-audio as `&CStr`.
fn cf_key(k: &CStr) -> CFRetained<CFString> {
    // Going through Rust str is the simplest path (the keys are short
    // ASCII identifiers). CFString::from_str handles UTF-8.
    let s = k.to_str().expect("CoreAudio key is ASCII");
    CFString::from_str(s)
}

/// Untyped `CFDictionarySetValue` wrapper. Bypasses the typed
/// `CFMutableDictionary::set` so we can mix value kinds (`CFString`,
/// `CFBoolean`, `CFArray`) in one dict without a generic-parameter war.
///
/// # Safety
/// `dict` must be a valid `CFMutableDictionary`. `key` and `value` must
/// be `CFType` pointers valid for the duration of this call. `CFMutableDictionary`
/// retains both internally, so the caller's local references stay valid.
/// The generic parameters on the typed `CFMutableDictionary` are erased here
/// — the underlying Core Foundation object is identical regardless of K/V.
unsafe fn dict_set<K: ?Sized, V: ?Sized>(
    dict: &CFMutableDictionary<K, V>,
    key: *const c_void,
    value: *const c_void,
) {
    // The typed CFMutableDictionary<K, V> and the default
    // CFMutableDictionary<Opaque, Opaque> share an ABI — they're the same
    // C struct. The generics are a Rust-only convenience for the typed
    // accessor methods. Reinterpret to the untyped variant before calling
    // the raw setter.
    let raw = (dict as *const CFMutableDictionary<K, V>).cast::<CFMutableDictionary>();
    unsafe {
        CFMutableDictionary::set_value(Some(&*raw), key, value);
    }
}

/// Translate a POSIX pid into a Core Audio process `AudioObjectID`. The
/// HAL exposes processes as audio objects (one per running app that has
/// touched Core Audio); `CATapDescription` expects these object IDs in its
/// exclude / include lists, not raw pids.
///
/// Returns None on translation failure (process hasn't yet been seen by
/// the HAL, or the API call rejected the qualifier).
fn translate_pid_to_process_object(pid: i32) -> Option<u32> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut object_id: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let pid_qualifier: i32 = pid;
    // SAFETY: Apple's TranslatePIDToProcessObject takes the pid as a
    // qualifier and writes the matching object id into outData. All
    // pointers are valid stack pointers for the duration of the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            // kAudioObjectSystemObject = 1
            1,
            NonNull::new(&mut addr).unwrap(),
            std::mem::size_of::<i32>() as u32,
            std::ptr::from_ref(&pid_qualifier).cast::<c_void>(),
            NonNull::new(&mut size).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut object_id).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 || object_id == 0 {
        return None;
    }
    Some(object_id)
}

/// Query the system's current default output device and return its UID
/// (e.g. `"BuiltInSpeakerDevice"`). Used to give the tap-only aggregate a
/// stable clock source. Currently unused: tests showed the aggregate
/// produces silent buffers when a main sub-device is set, so we leave
/// it out and let the tap clock itself. Kept here so we can re-enable
/// it if the macOS behaviour changes.
#[allow(dead_code)]
fn default_output_uid() -> Result<CFRetained<CFString>> {
    // Resolve the default output device's AudioObjectID.
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let addr_ptr = NonNull::new(&mut addr).unwrap();
    let mut dev_id: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let size_ptr = NonNull::new(&mut size).unwrap();
    let status = unsafe {
        AudioObjectGetPropertyData(
            // kAudioObjectSystemObject = 1
            1,
            addr_ptr,
            0,
            std::ptr::null(),
            size_ptr,
            NonNull::new(std::ptr::from_mut(&mut dev_id).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 || dev_id == 0 {
        return Err(anyhow!(
            "AudioObjectGetPropertyData(kAudioHardwarePropertyDefaultOutputDevice) \
             failed (status={status})"
        ));
    }

    // Resolve that device's UID (a CFString).
    let mut uid_addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let uid_addr_ptr = NonNull::new(&mut uid_addr).unwrap();
    let mut uid_size = std::mem::size_of::<*const CFString>() as u32;
    let uid_size_ptr = NonNull::new(&mut uid_size).unwrap();
    let mut uid_ptr: *const CFString = std::ptr::null();
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev_id,
            uid_addr_ptr,
            0,
            std::ptr::null(),
            uid_size_ptr,
            NonNull::new(std::ptr::from_mut(&mut uid_ptr).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 || uid_ptr.is_null() {
        return Err(anyhow!(
            "AudioObjectGetPropertyData(kAudioDevicePropertyDeviceUID) failed \
             (status={status})"
        ));
    }
    let nn = NonNull::new(uid_ptr.cast_mut()).unwrap();
    // SAFETY: HAL hands us a +1 retained CFString for this property.
    Ok(unsafe { CFRetained::from_raw(nn) })
}

/// Build the `CFDictionary` that describes the aggregate device wrapping the
/// tap. Keys come from `<CoreAudio/AudioHardware.h>` — we use the constants
/// objc2-core-audio re-exports. `main_sub_uid` is the UID of an existing
/// real audio device used purely as the clock source; without it a
/// tap-only aggregate has no time base and produces silence.
fn build_aggregate_dict(
    tap_uid: &CFString,
    main_sub_uid: Option<&CFString>,
) -> CFRetained<CFMutableDictionary<CFString, CFType>> {
    let dict: CFRetained<CFMutableDictionary<CFString, CFType>> =
        CFMutableDictionary::<CFString, CFType>::with_capacity(5);

    // Sub-tap entry:
    //   { kAudioSubTapUIDKey: tap_uid,
    //     kAudioSubTapDriftCompensationKey: true }
    //
    // Drift compensation is REQUIRED. Without it, the aggregate cannot
    // align the tap's audio clock with the main sub-device's clock and the
    // tap's IOProc receives zero-filled buffers (every callback fires at
    // the right rate but every sample is 0.0). Apple's WWDC23
    // CapturingSystemAudio sample sets this true; we hit exactly the
    // silent-buffer bug when it was missing.
    let sub_tap_dict: CFRetained<CFMutableDictionary<CFString, CFType>> =
        CFMutableDictionary::<CFString, CFType>::with_capacity(2);
    let sub_uid_key = cf_key(kAudioSubTapUIDKey);
    let sub_drift_key = cf_key(kAudioSubTapDriftCompensationKey);
    let drift_true = unsafe { kCFBooleanTrue }.expect("kCFBooleanTrue exists");
    unsafe {
        dict_set(
            &sub_tap_dict,
            CFRetained::as_ptr(&sub_uid_key).as_ptr() as *const c_void,
            (tap_uid as *const CFString).cast::<c_void>(),
        );
        dict_set(
            &sub_tap_dict,
            CFRetained::as_ptr(&sub_drift_key).as_ptr() as *const c_void,
            std::ptr::from_ref(drift_true).cast::<c_void>(),
        );
    }

    // Tap list: a CFArray of one dict (CFType element type).
    let sub_as_cf: &CFType = unsafe {
        &*(&*sub_tap_dict as *const CFMutableDictionary<CFString, CFType>).cast::<CFType>()
    };
    let tap_list: CFRetained<CFArray<CFType>> = CFArray::from_objects(&[sub_as_cf]);

    let agg_name = CFString::from_str(TAP_DEVICE_NAME);
    let agg_uid = CFString::from_str(AGGREGATE_DEVICE_UID);
    let priv_true = unsafe { kCFBooleanTrue }.expect("kCFBooleanTrue exists");
    let priv_false = unsafe { kCFBooleanFalse }.expect("kCFBooleanFalse exists");

    let name_k = cf_key(kAudioAggregateDeviceNameKey);
    let uid_k = cf_key(kAudioAggregateDeviceUIDKey);
    let priv_k = cf_key(kAudioAggregateDeviceIsPrivateKey);
    let stacked_k = cf_key(kAudioAggregateDeviceIsStackedKey);
    let taplist_k = cf_key(kAudioAggregateDeviceTapListKey);
    let main_k = cf_key(kAudioAggregateDeviceMainSubDeviceKey);
    let autostart_k = cf_key(kAudioAggregateDeviceTapAutoStartKey);

    unsafe {
        dict_set(
            &dict,
            CFRetained::as_ptr(&name_k).as_ptr() as *const c_void,
            CFRetained::as_ptr(&agg_name).as_ptr() as *const c_void,
        );
        dict_set(
            &dict,
            CFRetained::as_ptr(&uid_k).as_ptr() as *const c_void,
            CFRetained::as_ptr(&agg_uid).as_ptr() as *const c_void,
        );
        dict_set(
            &dict,
            CFRetained::as_ptr(&priv_k).as_ptr() as *const c_void,
            std::ptr::from_ref(priv_true).cast::<c_void>(),
        );
        dict_set(
            &dict,
            CFRetained::as_ptr(&stacked_k).as_ptr() as *const c_void,
            std::ptr::from_ref(priv_false).cast::<c_void>(),
        );
        dict_set(
            &dict,
            CFRetained::as_ptr(&taplist_k).as_ptr() as *const c_void,
            CFRetained::as_ptr(&tap_list).as_ptr() as *const c_void,
        );
        // TapAutoStart=NO: we explicitly call AudioDeviceStart on the
        // aggregate. AutoStart=YES caused interaction issues with
        // explicit IOProc management in sudara's working example, so we
        // match their pattern.
        dict_set(
            &dict,
            CFRetained::as_ptr(&autostart_k).as_ptr() as *const c_void,
            std::ptr::from_ref(priv_false).cast::<c_void>(),
        );
        if let Some(uid) = main_sub_uid {
            dict_set(
                &dict,
                CFRetained::as_ptr(&main_k).as_ptr() as *const c_void,
                (uid as *const CFString).cast::<c_void>(),
            );
        }
    }

    dict
}

// ── Private TCC API: explicit audio-capture permission request ─────────────
//
// macOS gates `AudioHardwareCreateProcessTap` behind a TCC service
// (`kTCCServiceAudioCapture`) that has no public preflight/request API.
// Without an explicit prompt the tap silently returns zero-filled buffers,
// even when the user has approved the bundle's `NSAudioCaptureUsageDescription`
// elsewhere — there's no implicit "ask on first use" hook for taps.
//
// The TCC.framework PrivateFrameworks bundle exposes two C functions used
// by Apple's own sample code (insidegui/AudioCap):
//   - `TCCAccessPreflight(service, options)` → status (0=allowed, 1=denied)
//   - `TCCAccessRequest(service, options, completion)` → prompts the user
//
// We resolve them via dlopen/dlsym so the build doesn't have to link the
// private framework directly (which Xcode flags as a private-API violation).
// The symbol search is best-effort: on a macOS where TCC moved or renamed
// the symbols, we log and continue — the user can still grant manually via
// System Settings.

use objc2_core_foundation::CFString as TccCFString;
use std::os::raw::c_int;

type TccPreflightFn = unsafe extern "C" fn(*const TccCFString, *const c_void) -> c_int;

/// Best-effort preflight of the audio-capture TCC service so we can LOG
/// what state the user is in (allowed / denied / not-determined). We
/// intentionally don't call `TCCAccessRequest` here — Apple expects that
/// function's completion handler to be an Objective-C block, not a plain
/// C function pointer, and a mismatch crashes the daemon at startup.
/// The popup fires anyway when `AudioHardwareCreateProcessTap` runs;
/// preflight is purely diagnostic.
fn request_audio_capture_permission() {
    unsafe {
        let lib_path = c"/System/Library/PrivateFrameworks/TCC.framework/TCC".as_ptr();
        let handle = libc::dlopen(lib_path, libc::RTLD_LAZY | libc::RTLD_GLOBAL);
        if handle.is_null() {
            warn!("TCC.framework dlopen failed — cannot probe audio-capture status");
            return;
        }
        let pre = libc::dlsym(handle, c"TCCAccessPreflight".as_ptr());
        if pre.is_null() {
            warn!("TCCAccessPreflight symbol not found");
            return;
        }
        let preflight: TccPreflightFn = std::mem::transmute(pre);
        let service = TccCFString::from_static_str("kTCCServiceAudioCapture");
        let status = preflight(&*service, std::ptr::null());
        match status {
            0 => info!("kTCCServiceAudioCapture: ALLOWED (tap will deliver audio)"),
            1 => warn!(
                "kTCCServiceAudioCapture: DENIED — open System Settings → Privacy & \
                 Security → Audio Recording (or Screen Recording on macOS 15+) and \
                 toggle Resonance ON"
            ),
            other => info!(
                "kTCCServiceAudioCapture: not-determined (preflight={other}) — \
                 macOS should prompt during tap creation"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_uid_is_reverse_dns() {
        assert!(AGGREGATE_DEVICE_UID.starts_with("com."));
    }

    #[test]
    fn tap_device_name_is_non_empty() {
        assert!(!TAP_DEVICE_NAME.is_empty());
    }
}
