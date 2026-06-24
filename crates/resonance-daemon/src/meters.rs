//! Lock-free meters shared between the audio RT thread (writer) and the IPC
//! thread (reader). Floats are stored as their bit patterns in `AtomicU32`, so
//! the RT thread never blocks or allocates.

use resonance_ipc::Meters;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// One block's worth of measurements, produced on the RT thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sample {
    pub in_peak: f32,
    pub out_peak: f32,
    pub in_rms: f32,
    pub out_rms: f32,
    pub clip: bool,
    pub dsp_load: f32,
    pub dsp_frame_us: u32,
}

#[derive(Default)]
pub struct AtomicMeters {
    in_peak: AtomicU32,
    out_peak: AtomicU32,
    in_rms: AtomicU32,
    out_rms: AtomicU32,
    /// Latched: set by the RT thread on clip, cleared when read.
    clip: AtomicBool,
    dsp_load: AtomicU32,
    dsp_frame_us: AtomicU32,
    /// The live DSP sample rate the RT thread is actually running at, in Hz
    /// (rounded). 0 until the backend reports one. Lets `status` show the rate
    /// the audio path negotiated — which may differ from the daemon's mirror
    /// chain after a device/graph rate change — so the pitch bug is diagnosable.
    live_sample_rate: AtomicU32,
    /// The live capture-side rate in Hz (pre-resample). Equal to
    /// `live_sample_rate` when no resampling is happening; differs when a backend
    /// converts the capture clock to the DSP/output clock (e.g. a macOS tap).
    live_capture_rate: AtomicU32,
}

impl AtomicMeters {
    /// Record the live DSP sample rate (RT thread). Cheap relaxed store.
    pub fn set_sample_rate(&self, hz: f64) {
        self.live_sample_rate
            .store(hz.round().max(0.0) as u32, Ordering::Relaxed);
    }

    /// The live DSP sample rate in Hz, or `None` if no backend has reported one.
    pub fn sample_rate(&self) -> Option<f64> {
        match self.live_sample_rate.load(Ordering::Relaxed) {
            0 => None,
            hz => Some(hz as f64),
        }
    }

    /// Record the live capture-side rate (RT thread / stream setup).
    pub fn set_capture_rate(&self, hz: f64) {
        self.live_capture_rate
            .store(hz.round().max(0.0) as u32, Ordering::Relaxed);
    }

    /// The live capture rate in Hz, or `None` if no backend has reported one.
    pub fn capture_rate(&self) -> Option<f64> {
        match self.live_capture_rate.load(Ordering::Relaxed) {
            0 => None,
            hz => Some(hz as f64),
        }
    }

    /// Publish a block's measurements (RT thread). Clip latches until read.
    pub fn store(&self, s: Sample) {
        self.in_peak.store(s.in_peak.to_bits(), Ordering::Relaxed);
        self.out_peak.store(s.out_peak.to_bits(), Ordering::Relaxed);
        self.in_rms.store(s.in_rms.to_bits(), Ordering::Relaxed);
        self.out_rms.store(s.out_rms.to_bits(), Ordering::Relaxed);
        self.dsp_load.store(s.dsp_load.to_bits(), Ordering::Relaxed);
        self.dsp_frame_us.store(s.dsp_frame_us, Ordering::Relaxed);
        if s.clip {
            self.clip.store(true, Ordering::Relaxed);
        }
    }

    /// Read current meters, clearing the latched clip flag (IPC thread).
    pub fn snapshot(&self) -> Meters {
        Meters {
            in_peak: f32::from_bits(self.in_peak.load(Ordering::Relaxed)),
            out_peak: f32::from_bits(self.out_peak.load(Ordering::Relaxed)),
            in_rms: f32::from_bits(self.in_rms.load(Ordering::Relaxed)),
            out_rms: f32::from_bits(self.out_rms.load(Ordering::Relaxed)),
            clip: self.clip.swap(false, Ordering::Relaxed),
            dsp_load: f32::from_bits(self.dsp_load.load(Ordering::Relaxed)),
            dsp_frame_us: self.dsp_frame_us.load(Ordering::Relaxed),
        }
    }
}

/// Peak (max |x|) and RMS of an interleaved stereo `f64` buffer, as `f32`.
pub fn peak_rms(buf: &[f64]) -> (f32, f32) {
    if buf.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f64;
    let mut sumsq = 0.0f64;
    for &x in buf {
        let a = x.abs();
        if a > peak {
            peak = a;
        }
        sumsq += x * x;
    }
    let rms = (sumsq / buf.len() as f64).sqrt();
    (peak as f32, rms as f32)
}

/// Peak/RMS of an interleaved stereo `f32` slice (the raw input buffers).
pub fn peak_rms_f32(buf: &[f32]) -> (f32, f32) {
    if buf.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f32;
    let mut sumsq = 0.0f64;
    for &x in buf {
        let a = x.abs();
        if a > peak {
            peak = a;
        }
        sumsq += (x as f64) * (x as f64);
    }
    let rms = (sumsq / buf.len() as f64).sqrt() as f32;
    (peak, rms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_rms_of_full_scale_sine_ish() {
        // A buffer of ±1.0 has peak 1.0 and RMS 1.0.
        let buf = [1.0f64, -1.0, 1.0, -1.0];
        let (p, r) = peak_rms(&buf);
        assert!((p - 1.0).abs() < 1e-6);
        assert!((r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn atomic_round_trip_and_clip_latch() {
        let m = AtomicMeters::default();
        m.store(Sample {
            in_peak: 0.5,
            out_peak: 0.8,
            clip: true,
            dsp_frame_us: 42,
            dsp_load: 0.1,
            ..Default::default()
        });
        let s1 = m.snapshot();
        assert!((s1.in_peak - 0.5).abs() < 1e-6);
        assert!((s1.out_peak - 0.8).abs() < 1e-6);
        assert_eq!(s1.dsp_frame_us, 42);
        assert!(s1.clip, "clip should be reported once");
        let s2 = m.snapshot();
        assert!(!s2.clip, "clip should clear after read");
    }
}
