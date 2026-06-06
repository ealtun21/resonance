//! Raw Core Audio HAL input for the tap aggregate device.
//!
//! cpal opens devices via AudioUnit (AUHAL). AUHAL is designed for hardware
//! input devices and does not reliably surface the tap's input stream on a
//! private aggregate device that wraps a `kAudioSubTapUIDKey` entry: it
//! ends up opening a real (but unused) sub-device's stream and reads
//! silence. The fix is to skip AUHAL entirely and register an
//! `AudioDeviceIOProcID` on the aggregate. The HAL fills the IOProc's
//! `inInputData` `AudioBufferList` straight from the tap.
//!
//! This module owns the IOProc registration + start/stop lifecycle, and
//! routes incoming samples into an SPSC ring buffer for the DSP/output
//! thread to consume — same contract the cpal input had before.

use anyhow::{Result, anyhow};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    kAudioDevicePropertyStreamFormat, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeInput,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatLinearPCM,
    kLinearPCMFormatFlagIsFloat, kLinearPCMFormatFlagIsNonInterleaved,
    kLinearPCMFormatFlagIsPacked,
};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

/// Number of stereo frames per IOProc callback we pre-allocate scratch for.
/// Apple's tap typically delivers 512–1024 frames; we size generously.
const MAX_FRAMES_PER_CYCLE: usize = 8192;

/// Owns an active `AudioDeviceIOProc` on the aggregate device. Drop
/// stops the device and unregisters the IOProc so the HAL forgets us.
pub struct HalInputStream {
    device_id: AudioObjectID,
    io_proc_id: AudioDeviceIOProcID,
    // Boxed state is kept on the heap so the raw pointer the IOProc
    // receives stays valid for the stream's lifetime even if we move
    // the struct around.
    _state: Box<IoState>,
    /// Total number of IOProc invocations — bumped from the audio thread.
    pub callback_count: Arc<AtomicU64>,
    /// IOProc invocations whose input buffer contained at least one
    /// non-zero sample — distinguishes "tap silent" from "tap not firing".
    pub nonzero_blocks: Arc<AtomicU64>,
}

struct IoState {
    /// SPSC ring producer the DSP thread reads from.
    ring_tx: rtrb::Producer<f32>,
    /// Format the aggregate device reports (sample rate, channel count,
    /// interleaved vs. planar). Captured at open time and used by the
    /// IOProc to decode the buffer layout.
    format: AudioStreamBasicDescription,
    callback_count: Arc<AtomicU64>,
    nonzero_blocks: Arc<AtomicU64>,
}

impl HalInputStream {
    /// Register an IOProc on the given aggregate device and start it.
    pub fn open(device_id: AudioObjectID, ring_tx: rtrb::Producer<f32>) -> Result<Self> {
        let format = query_input_format(device_id)?;
        info!(
            "tap aggregate input format: {} Hz, {} ch, {} bytes/frame, flags=0x{:x}",
            format.mSampleRate,
            format.mChannelsPerFrame,
            format.mBytesPerFrame,
            format.mFormatFlags
        );

        let callback_count = Arc::new(AtomicU64::new(0));
        let nonzero_blocks = Arc::new(AtomicU64::new(0));

        let state = Box::new(IoState {
            ring_tx,
            format,
            callback_count: Arc::clone(&callback_count),
            nonzero_blocks: Arc::clone(&nonzero_blocks),
        });
        let state_ptr: *mut IoState = Box::into_raw(state);
        // Re-box from the leaked pointer so we own the cleanup on Drop.
        // SAFETY: state_ptr came from Box::into_raw above, exclusively owned.
        let state_box: Box<IoState> = unsafe { Box::from_raw(state_ptr) };

        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: AudioDeviceCreateIOProcID stores the IOProc + client
        // pointer for later async invocation. state_ptr remains valid for
        // the lifetime of this struct (state_box keeps the alloc alive).
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device_id,
                Some(io_proc),
                state_ptr as *mut c_void,
                NonNull::new(&mut proc_id).unwrap(),
            )
        };
        if status != 0 || proc_id.is_none() {
            return Err(anyhow!(
                "AudioDeviceCreateIOProcID failed (status={status})"
            ));
        }

        // SAFETY: device_id + proc_id are valid; AudioDeviceStart starts
        // the HAL audio thread invoking our IOProc.
        let status = unsafe { AudioDeviceStart(device_id, proc_id) };
        if status != 0 {
            // Best-effort cleanup: destroy the IOProcID we just created.
            unsafe {
                let _ = AudioDeviceDestroyIOProcID(device_id, proc_id);
            }
            return Err(anyhow!("AudioDeviceStart failed (status={status})"));
        }

        info!(
            "HAL input stream started on aggregate device {device_id} (IOProcID={:?})",
            proc_id
        );

        Ok(Self {
            device_id,
            io_proc_id: proc_id,
            _state: state_box,
            callback_count,
            nonzero_blocks,
        })
    }
}

impl Drop for HalInputStream {
    fn drop(&mut self) {
        // Stop IO first so the HAL stops calling our IOProc, THEN tear it
        // down — otherwise we'd race the audio thread against the
        // deallocation of `_state`.
        // SAFETY: device_id + io_proc_id were valid when assigned; HAL
        // handles re-stop / re-destroy on already-gone targets gracefully.
        unsafe {
            let s1 = AudioDeviceStop(self.device_id, self.io_proc_id);
            if s1 != 0 {
                warn!(
                    "AudioDeviceStop failed (device={}, status={s1})",
                    self.device_id
                );
            }
            let s2 = AudioDeviceDestroyIOProcID(self.device_id, self.io_proc_id);
            if s2 != 0 {
                warn!(
                    "AudioDeviceDestroyIOProcID failed (device={}, status={s2})",
                    self.device_id
                );
            }
        }
    }
}

// SAFETY: HalInputStream owns a stable boxed IoState and an HAL handle.
// The HAL holds its own internal locks; AudioDeviceStop is sync and waits
// for the in-flight IOProc to drain before returning.
unsafe impl Send for HalInputStream {}
unsafe impl Sync for HalInputStream {}

/// The raw HAL IOProc. Called from a real-time audio thread owned by the
/// Core Audio HAL. Must NOT allocate, lock, or call back into Cocoa.
///
/// # Safety
/// Apple guarantees the buffer pointers, `inClientData`, and AudioBufferList
/// memory are valid for the duration of the call. `inClientData` is the
/// `*mut IoState` we registered via `AudioDeviceCreateIOProcID`.
unsafe extern "C-unwind" fn io_proc(
    _in_device: AudioObjectID,
    _in_now: NonNull<AudioTimeStamp>,
    in_input_data: NonNull<AudioBufferList>,
    _in_input_time: NonNull<AudioTimeStamp>,
    _out_output_data: NonNull<AudioBufferList>,
    _in_output_time: NonNull<AudioTimeStamp>,
    in_client_data: *mut c_void,
) -> i32 {
    if in_client_data.is_null() {
        return 0;
    }
    // SAFETY: in_client_data is our `*mut IoState`, kept alive by the
    // owning HalInputStream until AudioDeviceStop returns.
    let state = unsafe { &mut *(in_client_data as *mut IoState) };
    state.callback_count.fetch_add(1, Ordering::Relaxed);

    let bufs = unsafe { in_input_data.as_ref() };
    if bufs.mNumberBuffers == 0 {
        return 0;
    }

    let channels = state.format.mChannelsPerFrame as usize;
    let is_planar = (state.format.mFormatFlags & kLinearPCMFormatFlagIsNonInterleaved) != 0;

    // Walk the flexible array of AudioBuffer (mBuffers is declared as
    // [AudioBuffer; 1] but truly has mNumberBuffers entries).
    let mbuf_ptr = bufs.mBuffers.as_ptr();
    let mut any_nonzero = false;

    if is_planar {
        // Planar: one AudioBuffer per channel, each carrying nframes
        // mono f32 samples.
        if bufs.mNumberBuffers < 2 {
            // Mono input — duplicate to both channels.
            let b0 = unsafe { &*mbuf_ptr };
            if b0.mData.is_null() {
                return 0;
            }
            let frames = (b0.mDataByteSize as usize) / std::mem::size_of::<f32>();
            // SAFETY: HAL hands us at least mDataByteSize bytes of f32 data.
            let s0 = unsafe { std::slice::from_raw_parts(b0.mData as *const f32, frames) };
            for &v in s0 {
                if v != 0.0 {
                    any_nonzero = true;
                }
                let _ = state.ring_tx.push(v);
                let _ = state.ring_tx.push(v);
            }
        } else {
            // Stereo planar: buffers[0]=L, buffers[1]=R.
            let bl = unsafe { &*mbuf_ptr };
            let br = unsafe { &*mbuf_ptr.add(1) };
            if bl.mData.is_null() || br.mData.is_null() {
                return 0;
            }
            let frames = (bl.mDataByteSize as usize) / std::mem::size_of::<f32>();
            let sl = unsafe { std::slice::from_raw_parts(bl.mData as *const f32, frames) };
            let sr = unsafe { std::slice::from_raw_parts(br.mData as *const f32, frames) };
            for i in 0..frames {
                let l = sl[i];
                let r = sr[i];
                if l != 0.0 || r != 0.0 {
                    any_nonzero = true;
                }
                let _ = state.ring_tx.push(l);
                let _ = state.ring_tx.push(r);
            }
        }
    } else {
        // Interleaved: a single AudioBuffer with channels*frames samples.
        let b0 = unsafe { &*mbuf_ptr };
        if b0.mData.is_null() || channels == 0 {
            return 0;
        }
        let total = (b0.mDataByteSize as usize) / std::mem::size_of::<f32>();
        let frames = total / channels;
        let data = unsafe { std::slice::from_raw_parts(b0.mData as *const f32, frames * channels) };
        for i in 0..frames {
            let (l, r) = match channels {
                1 => {
                    let s = data[i];
                    (s, s)
                }
                _ => (data[i * channels], data[i * channels + 1]),
            };
            if l != 0.0 || r != 0.0 {
                any_nonzero = true;
            }
            let _ = state.ring_tx.push(l);
            let _ = state.ring_tx.push(r);
        }
    }

    if any_nonzero {
        state.nonzero_blocks.fetch_add(1, Ordering::Relaxed);
    }
    let _ = MAX_FRAMES_PER_CYCLE; // bound documentation; not enforced here

    0
}

/// Read the aggregate device's INPUT-scope StreamFormat so the IOProc knows
/// the channel count + interleaving layout it'll receive.
fn query_input_format(device_id: AudioObjectID) -> Result<AudioStreamBasicDescription> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamFormat,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut asbd: AudioStreamBasicDescription = AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kLinearPCMFormatFlagIsFloat | kLinearPCMFormatFlagIsPacked,
        mBytesPerPacket: 0,
        mFramesPerPacket: 1,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 2,
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::new(&mut addr).unwrap(),
            0,
            std::ptr::null(),
            NonNull::new(&mut size).unwrap(),
            NonNull::new(&mut asbd as *mut _ as *mut c_void).unwrap(),
        )
    };
    if status != 0 {
        return Err(anyhow!(
            "AudioObjectGetPropertyData(kAudioDevicePropertyStreamFormat/Input) \
             failed (status={status})"
        ));
    }
    Ok(asbd)
}
