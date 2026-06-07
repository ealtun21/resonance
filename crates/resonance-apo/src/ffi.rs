//! C ABI consumed by the C++ APO shell (`cpp/resonance_apo.cpp`).
//!
//! The shell owns COM/aggregation and the audio-engine lifecycle; it calls these
//! functions to do the DSP and to track the daemon's live parameters. It also
//! produces meters + a spectrum for the UI — but ONLY while the daemon flags
//! that a client is watching (`telemetry_enabled` in the shared file). When no
//! one is watching, the audio thread does nothing beyond a single atomic load.
//!
//! Every entry point catches panics (the crate's `apo` cargo profile keeps
//! `panic = "unwind"`) so a Rust panic can never unwind into `audiodg.exe`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use resonance_dsp::chain::ProcessorChain;
use rustfft::{FftPlanner, num_complex::Complex};

use crate::log;
use crate::state::{self, ChainSnapshot, SharedFile, TELEMETRY_BINS};

const RING: usize = 4096;
const FFT_SIZE: usize = 4096;
const FREQ_MIN: f64 = 25.0;
const FREQ_MAX: f64 = 20000.0;

/// Valid only between `lock` and `unlock`.
struct Locked {
    chain: ProcessorChain,
    /// Pre-allocated f64 work buffer so the RT path never allocates.
    scratch: Vec<f64>,
}

struct Shared {
    state: Mutex<Option<Locked>>,
    channels: AtomicUsize,
    sample_rate_bits: AtomicU64,
    stop: AtomicBool,

    // Telemetry (only touched when `telemetry_on`).
    telemetry_on: AtomicBool,
    in_peak: AtomicU32,
    out_peak: AtomicU32,
    in_rms: AtomicU32,
    out_rms: AtomicU32,
    /// Lock-free "latest samples" ring (mono mix) the RT thread fills and the
    /// worker reads for the FFT. AtomicU32 holds f32 bits.
    ring: [AtomicU32; RING],
    ring_pos: AtomicUsize,
}

/// Opaque handle returned to the C++ shell.
pub struct ApoEngine {
    shared: Arc<Shared>,
}

fn build_chain(snap: Option<&ChainSnapshot>, channels: usize, sample_rate: f64) -> ProcessorChain {
    match snap {
        Some(s) => s.build_chain(channels, sample_rate),
        None => ProcessorChain::builder()
            .channels(channels)
            .sample_rate(sample_rate)
            .build(),
    }
}

/// Worker thread: rebuild the chain on daemon changes, and (only when a client
/// is watching) compute the spectrum off the RT thread and publish telemetry.
fn worker_loop(weak: Weak<Shared>) {
    let path = state::default_state_path();
    let mut file: Option<SharedFile> = None;
    let mut last_gen: u64 = u64::MAX;

    // FFT setup (cheap to keep around; only used while watching).
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| {
            0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / FFT_SIZE as f64).cos()) as f32
        })
        .collect();
    let edges: Vec<f64> = (0..=TELEMETRY_BINS)
        .map(|i| {
            let t = i as f64 / TELEMETRY_BINS as f64;
            FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
        })
        .collect();
    let mut envelope = [0.0f32; TELEMETRY_BINS];
    let mut scratch = vec![Complex::new(0.0f32, 0.0); FFT_SIZE];
    let mut logged_open_err = false;

    loop {
        std::thread::sleep(Duration::from_millis(25));
        let Some(shared) = weak.upgrade() else {
            return;
        };
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        if file.is_none() {
            match SharedFile::open(&path) {
                Ok(f) => {
                    crate::log::line(&format!("state file opened: {}", path.display()));
                    file = Some(f);
                }
                Err(e) => {
                    // Log once: an ACCESS_DENIED here (audiodg as LocalService vs a
                    // user-created state file without inherited write ACL) means the
                    // APO runs but never reads the chain — no EQ, no spectrum.
                    if !logged_open_err {
                        logged_open_err = true;
                        crate::log::line(&format!(
                            "state file open FAILED: {} ({e})",
                            path.display()
                        ));
                    }
                }
            }
        }
        let Some(f) = file.as_ref() else {
            continue;
        };

        // Mirror the daemon's "is a client watching" gate to the RT thread.
        let want = f.telemetry_enabled();
        shared.telemetry_on.store(want, Ordering::Release);

        // Rebuild the chain when the daemon publishes new params.
        let cur = f.generation();
        if cur != last_gen {
            let channels = shared.channels.load(Ordering::Acquire);
            if channels != 0 {
                let sr = f64::from_bits(shared.sample_rate_bits.load(Ordering::Acquire));
                if let Some(snap) = f.read() {
                    // Update in place to preserve filter/effect state (click-free
                    // live edits). Only a structural change (band added/removed)
                    // needs a rebuild, which we do outside the lock.
                    let mut need_rebuild = false;
                    if let Ok(mut g) = shared.state.lock() {
                        if let Some(l) = g.as_mut() {
                            need_rebuild = !snap.apply_to(&mut l.chain, sr);
                        }
                    }
                    if need_rebuild {
                        let c = build_chain(Some(&snap), channels, sr);
                        if let Ok(mut g) = shared.state.lock() {
                            if let Some(l) = g.as_mut() {
                                l.chain = c;
                            }
                        }
                    }
                    last_gen = cur;
                }
            }
        }

        // Telemetry only while watched.
        if !want {
            continue;
        }
        let bins = compute_spectrum(&shared, &fft, &window, &edges, &mut envelope, &mut scratch);
        f.write_telemetry(
            f32::from_bits(shared.in_peak.load(Ordering::Relaxed)),
            f32::from_bits(shared.out_peak.load(Ordering::Relaxed)),
            f32::from_bits(shared.in_rms.load(Ordering::Relaxed)),
            f32::from_bits(shared.out_rms.load(Ordering::Relaxed)),
            &bins,
        );
    }
}

/// Read the latest `FFT_SIZE` samples from the ring and produce normalised,
/// log-spaced band energies (matches the daemon's spectrum display).
fn compute_spectrum(
    shared: &Shared,
    fft: &std::sync::Arc<dyn rustfft::Fft<f32>>,
    window: &[f32],
    edges: &[f64],
    envelope: &mut [f32; TELEMETRY_BINS],
    buf: &mut [Complex<f32>],
) -> [f32; TELEMETRY_BINS] {
    let end = shared.ring_pos.load(Ordering::Acquire);
    let start = end.wrapping_sub(FFT_SIZE);
    for (i, slot) in buf.iter_mut().enumerate() {
        let s = f32::from_bits(shared.ring[start.wrapping_add(i) % RING].load(Ordering::Relaxed));
        *slot = Complex::new(s * window[i], 0.0);
    }
    fft.process(buf);

    // Assume 48 kHz-ish; the exact rate only shifts bin mapping slightly and the
    // display is log-scaled. Use the locked sample rate when available.
    let sr = f64::from_bits(shared.sample_rate_bits.load(Ordering::Relaxed)).max(8000.0);
    let bin_hz = sr / FFT_SIZE as f64;
    let mut out = [0.0f32; TELEMETRY_BINS];
    for (b, slot) in out.iter_mut().enumerate() {
        let lo = ((edges[b] / bin_hz).floor() as usize).min(FFT_SIZE / 2);
        let hi = (((edges[b + 1] / bin_hz).ceil() as usize).max(lo + 1)).min(FFT_SIZE / 2);
        let mut mag = 0.0f32;
        for v in &buf[lo..hi] {
            mag += v.norm();
        }
        let count = (hi - lo).max(1);
        mag /= count as f32;
        // Normalise (FFT_SIZE scale) and compress to ~0..1.
        let v = (mag / (FFT_SIZE as f32 * 0.25)).sqrt().min(1.0);
        let e = &mut envelope[b];
        *e = if v > *e { v } else { *e * 0.7 + v * 0.3 };
        *slot = *e;
    }
    out
}

#[inline]
fn peak_rms_f32(buf: &[f32]) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sumsq = 0.0f64;
    for &x in buf {
        let a = x.abs();
        if a > peak {
            peak = a;
        }
        sumsq += (x as f64) * (x as f64);
    }
    let rms = if buf.is_empty() {
        0.0
    } else {
        (sumsq / buf.len() as f64).sqrt() as f32
    };
    (peak, rms)
}

/// Create an engine + start the worker. Returns null on failure.
#[unsafe(no_mangle)]
pub extern "C" fn resonance_apo_create() -> *mut ApoEngine {
    catch_unwind(|| {
        let shared = Arc::new(Shared {
            state: Mutex::new(None),
            channels: AtomicUsize::new(0),
            sample_rate_bits: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            telemetry_on: AtomicBool::new(false),
            in_peak: AtomicU32::new(0),
            out_peak: AtomicU32::new(0),
            in_rms: AtomicU32::new(0),
            out_rms: AtomicU32::new(0),
            ring: core::array::from_fn(|_| AtomicU32::new(0)),
            ring_pos: AtomicUsize::new(0),
        });
        let weak = Arc::downgrade(&shared);
        std::thread::spawn(move || worker_loop(weak));
        Box::into_raw(Box::new(ApoEngine { shared }))
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Format is now fixed: build the chain from the daemon's current state.
#[unsafe(no_mangle)]
pub extern "C" fn resonance_apo_lock(
    p: *mut ApoEngine,
    channels: u32,
    sample_rate: f64,
    max_frames: u32,
) {
    if p.is_null() {
        return;
    }
    let eng = unsafe { &*p };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let ch = channels.max(1) as usize;
        eng.shared.channels.store(ch, Ordering::Release);
        eng.shared
            .sample_rate_bits
            .store(sample_rate.to_bits(), Ordering::Release);
        let snap = SharedFile::open(&state::default_state_path())
            .ok()
            .and_then(|f| f.read());
        let mut chain = build_chain(snap.as_ref(), ch, sample_rate);
        chain.reset();
        let scratch = vec![0.0f64; (max_frames as usize).saturating_mul(ch)];
        if let Ok(mut g) = eng.shared.state.lock() {
            *g = Some(Locked { chain, scratch });
        }
    }));
}

/// Tear down the locked chain.
#[unsafe(no_mangle)]
pub extern "C" fn resonance_apo_unlock(p: *mut ApoEngine) {
    if p.is_null() {
        return;
    }
    let eng = unsafe { &*p };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        eng.shared.channels.store(0, Ordering::Release);
        if let Ok(mut g) = eng.shared.state.lock() {
            *g = None;
        }
    }));
}

/// Process `frames` interleaved f32 frames in place (the RT path).
#[unsafe(no_mangle)]
pub extern "C" fn resonance_apo_process(
    p: *mut ApoEngine,
    buf: *mut f32,
    frames: u32,
    channels: u32,
) {
    if p.is_null() || buf.is_null() || frames == 0 {
        return;
    }
    let eng = unsafe { &*p };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let ch = channels.max(1) as usize;
        let want = (frames as usize).saturating_mul(ch);
        let Ok(mut g) = eng.shared.state.try_lock() else {
            return; // never blocks; pass through untouched
        };
        let Some(l) = g.as_mut() else {
            return;
        };
        let n = want.min(l.scratch.len());
        if n == 0 {
            return;
        }
        let samples = unsafe { std::slice::from_raw_parts_mut(buf, n) };

        // Metering is gated: a single relaxed load when nobody is watching.
        let telemetry = eng.shared.telemetry_on.load(Ordering::Relaxed);
        if telemetry {
            let (ip, ir) = peak_rms_f32(&samples[..n]);
            eng.shared.in_peak.store(ip.to_bits(), Ordering::Relaxed);
            eng.shared.in_rms.store(ir.to_bits(), Ordering::Relaxed);
        }

        for (d, s) in l.scratch[..n].iter_mut().zip(samples.iter()) {
            *d = *s as f64;
        }
        l.chain.process(&mut l.scratch[..n]);
        for (d, s) in samples.iter_mut().zip(l.scratch[..n].iter()) {
            *d = *s as f32;
        }

        if telemetry {
            let (op, or) = peak_rms_f32(&samples[..n]);
            eng.shared.out_peak.store(op.to_bits(), Ordering::Relaxed);
            eng.shared.out_rms.store(or.to_bits(), Ordering::Relaxed);
            // Feed the spectrum ring with the mono mix of the output.
            let frames_n = n / ch;
            let mut pos = eng.shared.ring_pos.load(Ordering::Relaxed);
            for f in 0..frames_n {
                let mut acc = 0.0f32;
                for c in 0..ch {
                    acc += samples[f * ch + c];
                }
                let mono = acc / ch as f32;
                eng.shared.ring[pos % RING].store(mono.to_bits(), Ordering::Relaxed);
                pos = pos.wrapping_add(1);
            }
            eng.shared.ring_pos.store(pos, Ordering::Release);
        }
    }));
}

/// Append a C string to the APO log (called by the C++ shell for diagnostics).
#[unsafe(no_mangle)]
pub extern "C" fn resonance_apo_log(msg: *const core::ffi::c_char) {
    if msg.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(s) = unsafe { core::ffi::CStr::from_ptr(msg) }.to_str() {
            log::line(s);
        }
    }));
}

/// Stop the worker and free the engine.
#[unsafe(no_mangle)]
pub extern "C" fn resonance_apo_destroy(p: *mut ApoEngine) {
    if p.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let eng = unsafe { Box::from_raw(p) };
        eng.shared.stop.store(true, Ordering::Release);
    }));
}
