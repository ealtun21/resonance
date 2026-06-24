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
use resonance_dsp::resample::StreamResampler;
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
    /// The tap's capture sample rate (Hz). Surfaced so the supervisor can report
    /// it for `status` (and tell whether the IOProc is resampling).
    pub capture_rate: f64,
}

struct IoState {
    /// SPSC ring producer the DSP thread reads from. Carries samples at the
    /// **output device rate** (post-resample), so the output callback can pop
    /// them 1:1 without a pitch shift.
    ring_tx: rtrb::Producer<f32>,
    /// Format the aggregate device reports (sample rate, channel count,
    /// interleaved vs. planar). Captured at open time and used by the
    /// IOProc to decode the buffer layout.
    format: AudioStreamBasicDescription,
    /// Converts the tap capture rate (`format.mSampleRate`) to the output
    /// device rate. Bypasses when the two already match. Stereo (the IOProc
    /// always emits L/R pairs). Without this, a tap clocked differently from
    /// the output device plays back at the wrong pitch.
    resampler: StreamResampler<f32>,
    /// Interleaved stereo f32 scratch the IOProc assembles before resampling —
    /// pre-allocated so the audio thread never allocates.
    in_scratch: Vec<f32>,
    callback_count: Arc<AtomicU64>,
    nonzero_blocks: Arc<AtomicU64>,
}

impl HalInputStream {
    /// Register an IOProc on the given aggregate device and start it.
    ///
    /// `output_rate` is the sample rate the playback side (and the DSP chain)
    /// runs at. The IOProc resamples the tap's capture rate to this rate before
    /// pushing into the ring, so capture/playback rate mismatches (BT codecs,
    /// 44.1 kHz DACs) don't shift pitch.
    pub fn open(
        device_id: AudioObjectID,
        ring_tx: rtrb::Producer<f32>,
        output_rate: f64,
    ) -> Result<Self> {
        let format = query_input_format(device_id)?;
        info!(
            "tap aggregate input format: {} Hz, {} ch, {} bytes/frame, flags=0x{:x}; \
             resampling capture → {output_rate} Hz",
            format.mSampleRate,
            format.mChannelsPerFrame,
            format.mBytesPerFrame,
            format.mFormatFlags
        );

        let callback_count = Arc::new(AtomicU64::new(0));
        let nonzero_blocks = Arc::new(AtomicU64::new(0));

        // Stereo resampler: the IOProc always emits L/R pairs into the ring.
        let resampler = StreamResampler::<f32>::new(format.mSampleRate, output_rate, 2);
        let state = Box::new(IoState {
            ring_tx,
            format,
            resampler,
            in_scratch: Vec::with_capacity(MAX_FRAMES_PER_CYCLE * 2),
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
            capture_rate: format.mSampleRate,
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

    // Assemble this block as interleaved stereo into the reusable scratch, then
    // resample (capture rate → output rate) and push the result into the ring.
    let scratch = &mut state.in_scratch;
    scratch.clear();

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
                scratch.push(v);
                scratch.push(v);
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
                scratch.push(l);
                scratch.push(r);
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
            scratch.push(l);
            scratch.push(r);
        }
    }

    if any_nonzero {
        state.nonzero_blocks.fetch_add(1, Ordering::Relaxed);
    }

    // Resample capture → output rate (bypasses to a direct copy when equal) and
    // forward to the ring. Split the borrow so `process` (which returns a slice
    // borrowing the resampler) and the scratch input don't alias `state` whole.
    let IoState {
        resampler,
        in_scratch,
        ring_tx,
        ..
    } = state;
    for &s in resampler.process(in_scratch) {
        let _ = ring_tx.push(s);
    }

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
