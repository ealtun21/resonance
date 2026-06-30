//! Native macOS system-audio capture via Core Audio's Process Tap API.
//!
//! Apple introduced [`CATapDescription`] + [`AudioHardwareCreateProcessTap`]
//! in macOS 14.2. They let an unprivileged user-space process *tap* the
//! audio that other processes send to the system mixer, without installing
//! a kernel extension or an `AudioServerPlugIn` (which is what `BlackHole` or
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
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectSetPropertyData, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyUID,
};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFMutableDictionary, CFRetained, CFString, CFType, kCFBooleanFalse,
    kCFBooleanTrue,
};
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

/// Name the aggregate device shows up as in cpal/Audio MIDI Setup. cpal
/// enumerates devices by name; we look it up by exactly this string.
pub const TAP_DEVICE_NAME: &str = "Resonance EQ Tap";
/// UID prefix for the aggregate device. Each aggregate gets a per-process,
/// per-creation-unique UID built from this (see `AGGREGATE_SEQ`): recreating the
/// tap on a device/rate change must not collide with the old aggregate's UID
/// before it is destroyed (`AudioHardwareCreateAggregateDevice` returns 'nope'
/// on a duplicate UID).
const AGGREGATE_DEVICE_UID: &str = "com.ealtun21.resonance.tap-aggregate";

/// Monotonic counter making each created aggregate's UID unique within the
/// process (combined with the pid).
static AGGREGATE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Owns the Core Audio objects backing the system tap. Drops them on scope
/// exit so the system goes back to normal routing even on panic.
pub struct SystemAudioTap {
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
    native_rate: f64,
}

impl SystemAudioTap {
    /// Create the tap + aggregate device, bound to the output device `device_uid`
    /// so the tap inherits that device's channel layout and sample rate. Returns
    /// Err if we lack permission, if the API is unavailable (pre-14.2 macOS), or
    /// if Core Audio rejects our description.
    pub fn create(device_uid: &str) -> Result<Self> {
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
        // Device-bound, non-mixdown tap: bind to the output device's stream so
        // the tap inherits its full channel layout AND sample rate, instead of
        // the stereo / 48 kHz system-mix mixdown that the global-tap initializers
        // force. "Excluding processes" keeps the global-minus-self semantics, so
        // the exclude list (our own process) still prevents the feedback loop.
        // Stream index 0 — "the format of the tap matches the format of this
        // stream" (Apple). SAFETY: a normal Objective-C alloc/init sequence;
        // exclude_arr / uid_ns outlive the call.
        let uid_ns = NSString::from_str(device_uid);
        let desc: Retained<CATapDescription> = unsafe {
            CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
                CATapDescription::alloc(),
                &exclude_arr,
                &uid_ns,
                0,
            )
        };
        unsafe {
            desc.setName(&NSString::from_str(TAP_DEVICE_NAME));
            // private = true: the tap is invisible to other Core Audio clients
            // (e.g. Audio MIDI Setup) so it doesn't pollute the UI.
            desc.setPrivate(true);
            // Muted: the tap captures the system audio and we re-render the
            // DSP-processed signal via the output stream, so the original must
            // not also reach the speakers (that caused audible doubling).
            desc.setMuteBehavior(CATapMuteBehavior::Muted);
        }

        let mut tap_id: AudioObjectID = 0;
        // SAFETY: tap_id is a stack u32, valid for write. desc lives for
        // the duration of this call. A non-zero status means the system
        // rejected the request (most commonly: missing TCC permission).
        let status =
            unsafe { AudioHardwareCreateProcessTap(Some(&desc), std::ptr::from_mut(&mut tap_id)) };
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

        // The aggregate's rate right after creation is the system-mix rate the
        // tap natively sources (typically 48 kHz). We can retune the aggregate
        // DOWN to a lower output rate (real content, no resample), but asking
        // for MORE frames/sec than the mix produces makes the tap under-deliver
        // and the output ring starves — so callers cap the forced rate here and
        // resample up above it.
        let native_rate = read_nominal_rate(agg_id).unwrap_or(48_000.0);
        info!(
            "Process tap ready (tap_id={tap_id}, aggregate_id={agg_id}, \
             aggregate_name='{TAP_DEVICE_NAME}', native_rate={native_rate} Hz)"
        );
        Ok(Self {
            tap_id,
            aggregate_id: agg_id,
            native_rate,
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

    /// The aggregate's native sample rate — the system-mix rate the tap sources
    /// (typically 48 kHz). Output rates at or below this can be captured
    /// directly (the tap downsamples internally, no resample on our side); above
    /// it the tap can't source enough frames per second, so the backend keeps
    /// the tap here and `hal_input` resamples up instead.
    pub fn native_rate(&self) -> f64 {
        self.native_rate
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
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    let mut data_size: u32 = std::mem::size_of::<*const CFString>() as u32;
    let size_ptr = NonNull::new(std::ptr::from_mut(&mut data_size)).unwrap();
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

/// Read a device's current nominal sample rate
/// (`kAudioDevicePropertyNominalSampleRate`). Returns `None` if the HAL rejects
/// the query or reports a non-positive rate.
fn read_nominal_rate(device_id: AudioObjectID) -> Option<f64> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyNominalSampleRate,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut rate: f64 = 0.0;
    let mut size = std::mem::size_of::<f64>() as u32;
    // SAFETY: addr/size/rate are valid stack pointers held for the call. The
    // selector returns a single f64 and `size` is initialised to its width.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::new(std::ptr::from_mut(&mut addr)).unwrap(),
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut rate).cast::<c_void>()).unwrap(),
        )
    };
    (status == 0 && rate > 0.0).then_some(rate)
}

/// Drive the tap aggregate device to `target_hz` so the Process Tap captures at
/// the output device's rate. This makes `hal_input`'s resampler a no-op
/// (bypass) instead of converting the tap's default 48 kHz to the device rate on
/// every block — the macOS path then sounds identical to the native 48 kHz one.
///
/// `CoreAudio` applies a nominal-rate change asynchronously, so after the set we
/// poll the read-back until it settles: the `IOProc`'s stream-format query runs
/// immediately afterwards and must observe the new rate. When the aggregate is
/// already at `target_hz` (the common 48 kHz case) this is a single read with no
/// wait. If the HAL refuses the change (older macOS, or an aggregate that pins
/// its rate) we log it and leave the rate untouched, so `hal_input` keeps
/// bridging the gap exactly as before — the tap never goes silent on failure.
pub fn set_aggregate_nominal_rate(aggregate_id: AudioObjectID, target_hz: f64) {
    // Already at the requested rate — no set, no settle wait.
    if matches!(read_nominal_rate(aggregate_id), Some(cur) if (cur - target_hz).abs() < 1.0) {
        return;
    }

    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyNominalSampleRate,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut want = target_hz;
    // SAFETY: addr + want are valid stack pointers; the selector takes a single
    // f64 input of the size we pass. A non-zero status means the HAL declined.
    let status = unsafe {
        AudioObjectSetPropertyData(
            aggregate_id,
            NonNull::new(std::ptr::from_mut(&mut addr)).unwrap(),
            0,
            std::ptr::null(),
            std::mem::size_of::<f64>() as u32,
            NonNull::new(std::ptr::from_mut(&mut want).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 {
        warn!(
            "set aggregate {aggregate_id} nominal rate → {target_hz} Hz failed \
             (status={status}); tap stays at {:?} Hz and hal_input will resample",
            read_nominal_rate(aggregate_id)
        );
        return;
    }

    // The change is asynchronous; wait (on this supervisor thread, never the RT
    // audio thread) for the device to converge before the IOProc reads its
    // format. Caps at ~500 ms; converges in a few × 10 ms in practice.
    for _ in 0..50 {
        if matches!(read_nominal_rate(aggregate_id), Some(now) if (now - target_hz).abs() < 1.0) {
            info!(
                "aggregate {aggregate_id} nominal rate now {target_hz} Hz — tap matches \
                 output, no resampling"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    warn!(
        "aggregate {aggregate_id} did not reach {target_hz} Hz (now {:?} Hz); \
         hal_input will resample the remaining gap",
        read_nominal_rate(aggregate_id)
    );
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
    let raw = std::ptr::from_ref(dict).cast::<CFMutableDictionary>();
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
            NonNull::new(std::ptr::from_mut(&mut addr)).unwrap(),
            std::mem::size_of::<i32>() as u32,
            std::ptr::from_ref(&pid_qualifier).cast::<c_void>(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut object_id).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 || object_id == 0 {
        return None;
    }
    Some(object_id)
}

/// The system's current default output device UID (e.g. `"BuiltInSpeakerDevice"`,
/// `"BlackHole16ch_UID"`). The tap binds to this device so it inherits the
/// device's channel layout and sample rate.
pub fn default_output_uid() -> Result<String> {
    // Resolve the default output device's AudioObjectID.
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let addr_ptr = NonNull::new(std::ptr::from_mut(&mut addr)).unwrap();
    let mut dev_id: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let size_ptr = NonNull::new(std::ptr::from_mut(&mut size)).unwrap();
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
    let uid_addr_ptr = NonNull::new(std::ptr::from_mut(&mut uid_addr)).unwrap();
    let mut uid_size = std::mem::size_of::<*const CFString>() as u32;
    let uid_size_ptr = NonNull::new(std::ptr::from_mut(&mut uid_size)).unwrap();
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
    let cf = unsafe { CFRetained::from_raw(nn) };
    Ok(cf.to_string())
}

/// The default output device's current nominal sample rate, or None on failure.
/// Lets the backend recreate the device-bound tap when the device's rate changes
/// (Audio MIDI Setup, BT codec renegotiation): the tap inherits the device rate
/// at creation, so a fresh tap keeps capture == output and the resampler
/// bypassed instead of converting a stale tap rate.
pub fn default_output_rate() -> Option<f64> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut dev_id: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    // SAFETY: stack pointers valid for the call; the selector returns one
    // AudioObjectID into dev_id.
    let status = unsafe {
        AudioObjectGetPropertyData(
            1, // kAudioObjectSystemObject
            NonNull::new(std::ptr::from_mut(&mut addr)).unwrap(),
            0,
            std::ptr::null(),
            NonNull::new(std::ptr::from_mut(&mut size)).unwrap(),
            NonNull::new(std::ptr::from_mut(&mut dev_id).cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 || dev_id == 0 {
        return None;
    }
    read_nominal_rate(dev_id)
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
            std::ptr::from_ref(tap_uid).cast::<c_void>(),
        );
        dict_set(
            &sub_tap_dict,
            CFRetained::as_ptr(&sub_drift_key).as_ptr() as *const c_void,
            std::ptr::from_ref(drift_true).cast::<c_void>(),
        );
    }

    // Tap list: a CFArray of one dict (CFType element type).
    let sub_as_cf: &CFType = unsafe { &*std::ptr::from_ref(&*sub_tap_dict).cast::<CFType>() };
    let tap_list: CFRetained<CFArray<CFType>> = CFArray::from_objects(&[sub_as_cf]);

    let agg_name = CFString::from_str(TAP_DEVICE_NAME);
    // Per-process, per-creation-unique UID so recreating the tap never collides
    // with the not-yet-destroyed old aggregate.
    let pid = unsafe { libc::getpid() };
    let seq = AGGREGATE_SEQ.fetch_add(1, Ordering::Relaxed);
    let agg_uid = CFString::from_str(&format!("{AGGREGATE_DEVICE_UID}.{pid}.{seq}"));
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
                std::ptr::from_ref(uid).cast::<c_void>(),
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
        let status = preflight(std::ptr::from_ref(&*service), std::ptr::null());
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
