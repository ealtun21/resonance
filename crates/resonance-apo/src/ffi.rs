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
    /// Second buffer for the routing matrix output (square remap); same size.
    routed: Vec<f64>,
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
    /// worker reads for the FFT. `AtomicU32` holds f32 bits.
    ring: [AtomicU32; RING],
    ring_pos: AtomicUsize,
    /// `APOProcess` call counter, for throttled diagnostic logging.
    proc_calls: AtomicU64,
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

/// Attach the sidecar IR to a freshly built chain per the snapshot's
/// convolution fields. Kernel preparation (resample + FFT) happens right here —
/// callers must be off the RT path (worker thread / format lock).
fn attach_ir(
    chain: &mut ProcessorChain,
    snap: &ChainSnapshot,
    ir: Option<&std::sync::Arc<resonance_dsp::convolution::IrData>>,
) {
    if snap.convolution_generation == 0 {
        return;
    }
    if let Some(ir) = ir {
        match chain.convolution.load_ir(ir.clone()) {
            Ok(()) => {
                chain.convolution.set_enabled(snap.convolution_enabled != 0);
                // Diagnostic: probe the prepared kernel with an impulse so the
                // effective response is visible in the log (sum ≈ DC gain).
                let mut probe = chain.convolution.clone();
                let mut buf = vec![0.0f64; 2048];
                buf[0] = 1.0;
                probe.process(&mut buf, 1);
                let sum: f64 = buf.iter().sum();
                let peak = buf.iter().fold(0.0f64, |a, &b| a.max(b.abs()));
                log::line(&format!(
                    "IR attached: taps {}, ir_rate {}, engine_rate {}, probe dc {sum:.4} peak {peak:.4}",
                    chain.convolution.info().map_or(0, |i| i.taps),
                    ir.sample_rate,
                    chain.sample_rate,
                ));
            }
            Err(e) => log::line(&format!("convolution IR rejected: {e}")),
        }
    }
}

/// Render + attach the linear-phase FIR realisation of the chain's static
/// bands. No-op unless the chain's mode is Linear (armed by `build_chain`
/// from the snapshot). Kernel synthesis is an FFT — callers must be off the
/// RT path (worker thread / format lock), same rule as [`attach_ir`]. On a
/// failed render the chain simply keeps its IIR bank (never silence).
fn attach_eq_fir(chain: &mut ProcessorChain, sample_rate: f64) {
    if chain.phase_mode != resonance_dsp::chain::PhaseMode::Linear {
        return;
    }
    let ch = chain.channels;
    let Some(ir) = resonance_dsp::linphase::render(&chain.filters, ch, sample_rate) else {
        return; // no linearizable bands — IIR fallback is already correct
    };
    let taps = ir.channels.first().map_or(0, Vec::len);
    match chain.eq_fir.load_ir(std::sync::Arc::new(ir)) {
        Ok(()) => log::line(&format!(
            "linear-phase kernel attached: {taps} taps at {sample_rate} Hz"
        )),
        Err(e) => log::line(&format!("linear-phase kernel rejected: {e}")),
    }
}

/// Load the IR blob the snapshot references, or `None` (blob missing/corrupt →
/// run without convolution rather than erroring inside audiodg).
///
/// Retries briefly: on Windows a freshly renamed file can be transiently
/// unreadable (sharing violation while an on-access scanner holds it), which
/// would otherwise turn a valid IR into unity passthrough at format lock.
/// Callers are init/worker paths, never the RT callback.
fn load_ir_blob(
    snap: &ChainSnapshot,
) -> Option<std::sync::Arc<resonance_dsp::convolution::IrData>> {
    if snap.convolution_generation == 0 {
        return None;
    }
    let mut read = state::read_ir_blob(&state::default_ir_path());
    for _ in 0..5 {
        if read.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        read = state::read_ir_blob(&state::default_ir_path());
    }
    if let Some((generation, ir)) = read {
        log::line(&format!(
            "IR blob loaded: gen {generation}, {} ch, {} frames @ {} Hz",
            ir.channels.len(),
            ir.frames(),
            ir.sample_rate
        ));
        Some(std::sync::Arc::new(ir))
    } else {
        log::line("IR blob referenced by snapshot but unreadable - convolution off");
        None
    }
}

/// How long the daemon heartbeat may sit unchanged before the worker forces
/// a bypass. The daemon beats every ~30 ms; 2 s of silence means it is gone
/// (quit, killed, or crashed) — taskkill /f skips every shutdown hook, so
/// this staleness check is the only crash-safe teardown signal.
const STALE_AFTER: Duration = Duration::from_secs(2);

/// Tracks the daemon heartbeat and decides staleness. Pure logic (fed a
/// timestamp) so the bypass rule is unit-testable without a worker thread.
struct HeartbeatWatch {
    last_seen: Option<u64>,
    changed_at: Option<std::time::Instant>,
}

impl HeartbeatWatch {
    fn new() -> Self {
        Self {
            last_seen: None,
            changed_at: None,
        }
    }

    /// Feed the latest heartbeat reading (`None` = state file unreadable).
    /// Returns true once the value has not advanced for `STALE_AFTER`.
    fn observe(&mut self, hb: Option<u64>, now: std::time::Instant) -> bool {
        if self.changed_at.is_none() || hb != self.last_seen {
            self.last_seen = hb;
            self.changed_at = Some(now);
            return false;
        }
        self.changed_at
            .is_some_and(|t| now.duration_since(t) >= STALE_AFTER)
    }
}

/// Worker thread: rebuild the chain on daemon changes, and (only when a client
/// is watching) compute the spectrum off the RT thread and publish telemetry.
// `weak` is moved into and owned for the lifetime of this spawned worker thread.
#[allow(clippy::needless_pass_by_value)]
fn worker_loop(weak: Weak<Shared>) {
    const STARVE_TICKS: u32 = 16; // ~400 ms at the 25 ms worker tick
    let path = state::default_state_path();
    let mut file: Option<SharedFile> = None;
    let mut last_gen: u64 = u64::MAX;
    // Convolution IR cache: reloaded from the sidecar blob when the snapshot's
    // generation stamp changes. Kernel prep runs on THIS thread, never the RT.
    let mut last_ir_gen: u32 = 0;
    let mut cached_ir: Option<std::sync::Arc<resonance_dsp::convolution::IrData>> = None;

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
    // Spectrum-freeze guards (mirror the Linux `spectrum::run` task):
    //   `prev_want`    — clear the envelope once when the watch gate closes.
    //   `last_ring_pos`/`starved_ticks` — detect the APO process callback no
    //                    longer feeding the ring (endpoint change, audiodg
    //                    recycle) so we decay instead of re-FFTing a stale window.
    let mut prev_want = false;
    let mut last_ring_pos: usize = 0;
    let mut starved_ticks: u32 = 0;
    let mut watch = HeartbeatWatch::new();
    let mut was_stale = false;

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

        // Read the daemon's chain + "is a client watching" gate FRESH each poll.
        // A long-lived mapped view (`f`) does not observe the daemon's writes
        // across sessions on Windows; a fresh read of the file does. `f` is kept
        // only for telemetry *writes* below (the APO's own region).
        let _ = f;
        let fresh = crate::state::read_chain_fresh(&path);
        let want = fresh.as_ref().is_some_and(|(_, _, gate)| *gate);
        shared.telemetry_on.store(want, Ordering::Release);

        // Rebuild the chain when the daemon publishes new params.
        if let Some((cur, snap, _)) = fresh {
            if cur != last_gen {
                let channels = shared.channels.load(Ordering::Acquire);
                if channels != 0 {
                    let sr = f64::from_bits(shared.sample_rate_bits.load(Ordering::Acquire));
                    // An IR change (new blob generation) always takes the
                    // rebuild path: the kernel prep (resample + FFT) must run
                    // here on the worker, then swap in whole. Only latch the
                    // generation once the blob actually READ — a transient
                    // read failure (scanner holding the fresh file) must retry
                    // on the next tick, not disable convolution until the next
                    // IR change.
                    let ir_changed = snap.convolution_generation != last_ir_gen;
                    if ir_changed {
                        cached_ir = load_ir_blob(&snap);
                        if cached_ir.is_some() || snap.convolution_generation == 0 {
                            last_ir_gen = snap.convolution_generation;
                        }
                    }
                    // Update in place to preserve filter/effect state (click-free
                    // live edits). Only a structural change (band added/removed,
                    // IR presence/content change) needs a rebuild, done outside
                    // the lock.
                    let mut need_rebuild = ir_changed;
                    if !need_rebuild {
                        if let Ok(mut g) = shared.state.lock() {
                            if let Some(l) = g.as_mut() {
                                need_rebuild = !snap.apply_to(&mut l.chain, sr);
                            }
                        }
                    }
                    if need_rebuild {
                        let mut c = build_chain(Some(&snap), channels, sr);
                        attach_ir(&mut c, &snap, cached_ir.as_ref());
                        attach_eq_fir(&mut c, sr);
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

        // Daemon liveness: when the heartbeat stops advancing, force a
        // bypass so EQ never outlives its control plane. The daemon's
        // next publish (a generation change) rebuilds the chain and
        // restores normal processing.
        let hb = crate::state::read_heartbeat_fresh(&path);
        let stale = watch.observe(hb, std::time::Instant::now());
        if stale {
            if let Ok(mut g) = shared.state.try_lock() {
                if let Some(l) = g.as_mut() {
                    if l.chain.enabled {
                        l.chain.enabled = false;
                        crate::log::line("daemon heartbeat stale - forcing bypass");
                    }
                }
            }
        } else if was_stale {
            // Heartbeat resumed: force a snapshot re-read so `enabled` is
            // restored per the daemon's last publish. Never blindly
            // re-enable — after a graceful shutdown the snapshot itself
            // says enabled=0, and recovery requires an advancing
            // heartbeat, so a dead daemon can't revive EQ. `u64::MAX` is
            // unreachable as a real generation (they start near 0 and
            // increment by 2), so this unconditionally forces the rebuild
            // path above on the next tick.
            last_gen = u64::MAX;
        }
        was_stale = stale;

        // Telemetry only while watched.
        if !want {
            // Gate just closed: clear the smoothed envelope so a client that
            // reconnects starts from silence rather than the last frame.
            if prev_want {
                envelope.fill(0.0);
            }
            prev_want = false;
            continue;
        }
        prev_want = true;

        // Decay toward silence when the ring has stalled (the APO process
        // callback stopped feeding it) instead of re-FFTing a frozen window.
        // Debounced past any realistic callback gap.
        let ring_pos = shared.ring_pos.load(Ordering::Acquire);
        if ring_pos == last_ring_pos {
            starved_ticks = starved_ticks.saturating_add(1);
        } else {
            starved_ticks = 0;
            last_ring_pos = ring_pos;
        }
        let bins = if starved_ticks >= STARVE_TICKS {
            for e in &mut envelope {
                *e *= 0.7;
                if *e < 1e-4 {
                    *e = 0.0;
                }
            }
            envelope
        } else {
            compute_spectrum(&shared, &fft, &window, &edges, &mut envelope, &mut scratch)
        };
        f.write_telemetry(
            f32::from_bits(shared.in_peak.load(Ordering::Relaxed)),
            f32::from_bits(shared.out_peak.load(Ordering::Relaxed)),
            f32::from_bits(shared.in_rms.load(Ordering::Relaxed)),
            f32::from_bits(shared.out_rms.load(Ordering::Relaxed)),
            f64::from_bits(shared.sample_rate_bits.load(Ordering::Relaxed)) as f32,
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
        sumsq += f64::from(x) * f64::from(x);
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
            proc_calls: AtomicU64::new(0),
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
        // Format lock is initialisation, not the streaming callback — safe to
        // read + prepare the convolution IR here.
        if let Some(s) = snap.as_ref() {
            attach_ir(&mut chain, s, load_ir_blob(s).as_ref());
        }
        attach_eq_fir(&mut chain, sample_rate);
        chain.reset();
        let scratch = vec![0.0f64; (max_frames as usize).saturating_mul(ch)];
        let routed = scratch.clone();
        if let Ok(mut g) = eng.shared.state.lock() {
            *g = Some(Locked {
                chain,
                scratch,
                routed,
            });
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

        // Diagnostic (throttled ~1/sec at 48k): does THIS APO instance actually
        // receive real audio, and is the chain live? On endpoints where the APO
        // sits on a silent/bypassed connection, in_rms stays ~0 forever while
        // audio still plays — that distinguishes "APO not in the signal path"
        // from "APO in path but chain flat".
        let calls = eng.shared.proc_calls.fetch_add(1, Ordering::Relaxed);
        if calls % 100 == 0 {
            let (ipk, irms) = peak_rms_f32(&samples[..n]);
            crate::log::line(&format!(
                "process #{calls}: in_peak={ipk:.4} in_rms={irms:.4} frames={frames} ch={ch} enabled={} preamp={:.1} filters={}",
                l.chain.enabled,
                l.chain.preamp_db,
                l.chain.filters.len(),
            ));
        }

        for (d, s) in l.scratch[..n].iter_mut().zip(samples.iter()) {
            *d = f64::from(*s);
        }
        l.chain.process(&mut l.scratch[..n]);
        // Apply the output routing matrix (square remap) in place when present;
        // `route` is a square N→N map here, so frame count and length are
        // preserved. No routing → write the processed scratch straight back.
        let out: &[f64] = if l.chain.routing.is_some() {
            l.chain.route(&l.scratch[..n], &mut l.routed[..n]);
            &l.routed[..n]
        } else {
            &l.scratch[..n]
        };
        for (d, s) in samples.iter_mut().zip(out.iter()) {
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

#[cfg(test)]
mod hires_harness {
    //! Drives the REAL APO exports (create → lock → process → unlock → destroy)
    //! at a spread of fixed + pseudo-random sample rates, with no Windows audio
    //! endpoint/audiodg involved. Publishes a known chain — one +12 dB peak band
    //! at 1 kHz — then, for each rate, feeds a 1 kHz tone through the APO and
    //! asserts:
    //!   * the band still boosts 1 kHz by ~+12 dB  → the chain was built at the
    //!     correct rate (a rate bug would move the band off 1 kHz, dropping the
    //!     gain), and
    //!   * the output peak is still at 1 kHz        → no pitch shift.
    //!
    //! This is the on-VM stand-in for a real >48 kHz Windows endpoint (which the
    //! emulated HD-Audio codec can't provide): it exercises the exact shipping
    //! APO code path at hi-res.
    use super::{
        HeartbeatWatch, resonance_apo_create, resonance_apo_destroy, resonance_apo_lock,
        resonance_apo_process, resonance_apo_unlock,
    };
    use crate::state::{ApoStateWriter, default_state_path};
    use resonance_dsp::chain::ProcessorChain;
    use resonance_dsp::filter::{ApoFilter, FilterType};
    use rustfft::{FftPlanner, num_complex::Complex};
    use std::f64::consts::PI;

    /// Deterministic LCG — reproducible "random" rates across runs/CI.
    fn next_rand(s: &mut u64) -> u64 {
        *s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *s >> 33
    }

    /// Run a `tone_hz` sine through the APO at `rate`; return `(gain_db, peak_hz)`.
    fn measure(rate: f64, tone_hz: f64) -> (f64, f64) {
        let max_frames = 1024u32;
        let p = resonance_apo_create();
        assert!(!p.is_null(), "create returned null");
        resonance_apo_lock(p, 2, rate, max_frames);
        // Let the worker reach steady state before driving audio: the tight
        // process loop below runs faster than real time, so on a slow runner a
        // worker-side chain rebuild (25 ms tick) can otherwise land mid-tone
        // and split the analysis window across two chain instances.
        std::thread::sleep(std::time::Duration::from_millis(150));

        let frames = (rate * 0.5) as usize;
        let amp = 0.2f64; // +12 dB → 0.2·3.98 ≈ 0.8 peak, no clipping
        let w = 2.0 * PI * tone_hz / rate;
        let mut buf: Vec<f32> = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (amp * (w * i as f64).sin()) as f32;
            buf.push(s);
            buf.push(s);
        }
        let mut off = 0usize;
        while off < frames {
            let n = (frames - off).min(max_frames as usize);
            resonance_apo_process(p, buf[off * 2..].as_mut_ptr(), n as u32, 2);
            off += n;
        }

        // Steady-state mono (channel 0); skip the filter's settling transient.
        let skip = frames / 4;
        let mono: Vec<f64> = (skip..frames).map(|i| f64::from(buf[i * 2])).collect();
        let in_rms = amp / 2f64.sqrt();
        let out_rms = (mono.iter().map(|x| x * x).sum::<f64>() / mono.len() as f64).sqrt();
        let gain_db = 20.0 * (out_rms / in_rms).log10();

        // FFT peak (largest pow2 fitting the steady-state, capped) + parabolic
        // interpolation for sub-bin accuracy regardless of rate.
        let mut fft_n = 1usize;
        while fft_n * 2 <= mono.len().min(65536) {
            fft_n *= 2;
        }
        let seg = &mono[mono.len() - fft_n..];
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_n);
        let mut c: Vec<Complex<f64>> = seg
            .iter()
            .enumerate()
            .map(|(i, &x)| {
                let win = 0.5 * (1.0 - (2.0 * PI * i as f64 / fft_n as f64).cos());
                Complex::new(x * win, 0.0)
            })
            .collect();
        fft.process(&mut c);
        let half = fft_n / 2;
        let (mut bb, mut bm) = (1usize, 0.0f64);
        for (k, v) in c.iter().enumerate().take(half).skip(1) {
            let m = v.norm();
            if m > bm {
                bm = m;
                bb = k;
            }
        }
        let mut peak = bb as f64;
        if bb >= 1 && bb + 1 < half {
            let (a, b, cc) = (c[bb - 1].norm(), c[bb].norm(), c[bb + 1].norm());
            let denom = a - 2.0 * b + cc;
            if denom.abs() > f64::EPSILON {
                let d = 0.5 * (a - cc) / denom;
                if d.abs() <= 1.0 {
                    peak += d;
                }
            }
        }
        let peak_hz = peak * rate / fft_n as f64;

        resonance_apo_unlock(p);
        resonance_apo_destroy(p);
        (gain_db, peak_hz)
    }

    #[test]
    fn apo_eq_rate_correct_across_many_rates() {
        const TONE: f64 = 1_000.0;
        // Publish a known chain: one +12 dB peak band at 1 kHz, nothing else.
        let band = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(1000.0)
            .gain_db(12.0)
            .q(4.0)
            .enabled(true)
            .channels(2)
            .sample_rate(48_000.0)
            .build()
            .unwrap();
        let chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .add_filter(band)
            .build();
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }

        let mut rates: Vec<f64> = vec![
            16_000.0, 22_050.0, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0,
            192_000.0,
        ];
        // 8 pseudo-random rates in [8000, 384000] — arbitrary rates stress the
        // rate-agnostic chain build (w0 = 2π·f/sr for any sr).
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..8 {
            rates.push(8_000.0 + (next_rand(&mut seed) % 376_001) as f64);
        }

        for r in rates {
            assert!(
                TONE < r / 2.0 - 200.0,
                "tone must sit below Nyquist for {r}"
            );
            let (gain_db, peak_hz) = measure(r, TONE);
            assert!(
                (gain_db - 12.0).abs() < 1.5,
                "rate {r:.0}: 1 kHz gain {gain_db:.2} dB (want +12) — band shifted off 1 kHz \
                 (rate bug)?"
            );
            assert!(
                (peak_hz - TONE).abs() < 8.0,
                "rate {r:.0}: output peak {peak_hz:.1} Hz (want {TONE}) — pitch shift?"
            );
        }
    }

    #[test]
    fn apo_convolution_rate_mismatched_ir_is_transparent() {
        // The live-VM scenario that failed: a 96 kHz delta IR convolved by a
        // 48 kHz engine. The engine must resample the kernel (gain-compensated)
        // and stay ~0 dB — a broken downsample path shows up as a huge loss.
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .build();
        chain
            .convolution
            .load_ir(std::sync::Arc::new(resonance_dsp::convolution::IrData {
                name: "delta96".into(),
                path: "/delta96.wav".into(),
                sample_rate: 96_000.0,
                channels: vec![{
                    let mut v = vec![0.0; 64];
                    v[32] = 1.0;
                    v
                }],
            }))
            .unwrap();
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }
        let (gain_db, peak_hz) = measure(48_000.0, 1_000.0);
        assert!(
            gain_db.abs() < 1.0,
            "96k delta IR at 48k engine should be ~0 dB, got {gain_db:.2}"
        );
        assert!((peak_hz - 1_000.0).abs() < 8.0, "pitch intact: {peak_hz}");
        // Restore a flat default state for the following tests.
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(
                &ProcessorChain::builder()
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build(),
            );
        }
        let _ = std::fs::remove_file(crate::state::default_ir_path());
    }

    #[test]
    fn apo_convolution_ir_from_blob_is_applied() {
        // A single-tap 0.5 IR = a broadband −6.02 dB. Publish a flat chain with
        // it loaded; the APO must read the sidecar blob at format lock, prepare
        // the kernel and convolve — measured straight off the process path.
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48_000.0)
            .build();
        chain
            .convolution
            .load_ir(std::sync::Arc::new(resonance_dsp::convolution::IrData {
                name: "half".into(),
                path: "/half.wav".into(),
                sample_rate: 48_000.0,
                channels: vec![vec![0.5]],
            }))
            .unwrap();
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }
        let (gain_db, peak_hz) = measure(48_000.0, 1_000.0);
        assert!(
            (gain_db + 6.02).abs() < 0.5,
            "0.5-tap IR should measure ≈ −6 dB through the APO, got {gain_db:.2}"
        );
        assert!((peak_hz - 1_000.0).abs() < 8.0, "pitch intact: {peak_hz}");

        // Bypassed IR = unity again.
        chain.convolution.set_enabled(false);
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }
        let (gain_db, _) = measure(48_000.0, 1_000.0);
        assert!(
            gain_db.abs() < 0.5,
            "bypassed IR should measure ≈ 0 dB, got {gain_db:.2}"
        );

        // Clean up so later tests see a flat default state.
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(
                &ProcessorChain::builder()
                    .channels(2)
                    .sample_rate(48_000.0)
                    .build(),
            );
        }
        let _ = std::fs::remove_file(crate::state::default_ir_path());
    }

    // These share `default_state_path()` and global engine state with the rate
    // test, so run the APO tests serially: `cargo test -p resonance-apo --
    // --test-threads=1`.

    #[test]
    fn apo_per_channel_eq_targets_only_masked_channel() {
        use resonance_dsp::channel::ChannelMask;
        let rate = 48_000.0;
        // +12 dB @ 1 kHz, masked to channel 0 (L) only.
        let band = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(1000.0)
            .gain_db(12.0)
            .q(4.0)
            .enabled(true)
            .channels(2)
            .sample_rate(rate)
            .channel_mask(ChannelMask::single(0))
            .build()
            .unwrap();
        let chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(rate)
            .add_filter(band)
            .build();
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }

        let p = resonance_apo_create();
        assert!(!p.is_null());
        resonance_apo_lock(p, 2, rate, 1024); // lock reads the published state + builds with the mask

        let frames = (rate * 0.5) as usize;
        let amp = 0.2f64;
        let w = 2.0 * PI * 1000.0 / rate;
        let mut buf: Vec<f32> = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (amp * (w * i as f64).sin()) as f32;
            buf.push(s); // L
            buf.push(s); // R (identical input)
        }
        let mut off = 0usize;
        while off < frames {
            let n = (frames - off).min(1024);
            resonance_apo_process(p, buf[off * 2..].as_mut_ptr(), n as u32, 2);
            off += n;
        }
        resonance_apo_unlock(p);
        resonance_apo_destroy(p);

        let skip = frames / 4;
        let in_rms = amp / 2f64.sqrt();
        let chan_gain = |ch: usize| {
            let ms = (skip..frames)
                .map(|i| {
                    let x = f64::from(buf[i * 2 + ch]);
                    x * x
                })
                .sum::<f64>()
                / (frames - skip) as f64;
            20.0 * (ms.sqrt() / in_rms).log10()
        };
        let l = chan_gain(0);
        let r = chan_gain(1);
        assert!(
            (l - 12.0).abs() < 1.5,
            "masked channel 0 (L) should boost +12 dB, got {l:.2}"
        );
        assert!(
            r.abs() < 1.0,
            "unmasked channel 1 (R) should be ~0 dB, got {r:.2}"
        );
    }

    #[test]
    fn apo_per_channel_eq_targets_only_masked_channel_6ch() {
        use resonance_dsp::channel::ChannelMask;
        let rate = 48_000.0;
        let channels = 6usize;
        // +12 dB @ 1 kHz, masked to channel 3 only — exercises the APO's
        // N-channel (>2ch) per-channel path the macOS/PipeWire backends feed.
        let band = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(1000.0)
            .gain_db(12.0)
            .q(4.0)
            .enabled(true)
            .channels(channels)
            .sample_rate(rate)
            .channel_mask(ChannelMask::single(3))
            .build()
            .unwrap();
        let chain = ProcessorChain::builder()
            .channels(channels)
            .sample_rate(rate)
            .add_filter(band)
            .build();
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }

        let p = resonance_apo_create();
        assert!(!p.is_null());
        resonance_apo_lock(p, channels as u32, rate, 1024);

        let frames = (rate * 0.5) as usize;
        let amp = 0.2f64;
        let w = 2.0 * PI * 1000.0 / rate;
        // Identical 1 kHz tone on every channel.
        let mut buf: Vec<f32> = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            let s = (amp * (w * i as f64).sin()) as f32;
            for _ in 0..channels {
                buf.push(s);
            }
        }
        let mut off = 0usize;
        while off < frames {
            let n = (frames - off).min(1024);
            resonance_apo_process(
                p,
                buf[off * channels..].as_mut_ptr(),
                n as u32,
                channels as u32,
            );
            off += n;
        }
        resonance_apo_unlock(p);
        resonance_apo_destroy(p);

        let skip = frames / 4;
        let in_rms = amp / 2f64.sqrt();
        let chan_gain = |ch: usize| {
            let ms = (skip..frames)
                .map(|i| {
                    let x = f64::from(buf[i * channels + ch]);
                    x * x
                })
                .sum::<f64>()
                / (frames - skip) as f64;
            20.0 * (ms.sqrt() / in_rms).log10()
        };
        for ch in 0..channels {
            let g = chan_gain(ch);
            if ch == 3 {
                assert!(
                    (g - 12.0).abs() < 1.5,
                    "masked channel 3 should boost +12 dB, got {g:.2}"
                );
            } else {
                assert!(
                    g.abs() < 1.0,
                    "unmasked channel {ch} should be ~0 dB, got {g:.2}"
                );
            }
        }
    }

    #[test]
    fn apo_routing_swaps_channels() {
        use resonance_dsp::channel::ChannelMatrix;
        let rate = 48_000.0;
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(rate)
            .build();
        chain.routing = Some(ChannelMatrix::swap(2, 0, 1)); // L/R swap
        {
            let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
            w.publish(&chain);
        }

        let p = resonance_apo_create();
        assert!(!p.is_null());
        resonance_apo_lock(p, 2, rate, 1024);

        // Constant, distinct L/R so the swap is unambiguous.
        let frames = 256usize;
        let mut buf = vec![0.0f32; frames * 2];
        for i in 0..frames {
            buf[i * 2] = 0.5; // L
            buf[i * 2 + 1] = -0.5; // R
        }
        resonance_apo_process(p, buf.as_mut_ptr(), frames as u32, 2);
        resonance_apo_unlock(p);
        resonance_apo_destroy(p);

        let mid = frames / 2;
        assert!(
            (buf[mid * 2] - (-0.5)).abs() < 1e-3,
            "L should carry the swapped-in R (-0.5), got {}",
            buf[mid * 2]
        );
        assert!(
            (buf[mid * 2 + 1] - 0.5).abs() < 1e-3,
            "R should carry the swapped-in L (0.5), got {}",
            buf[mid * 2 + 1]
        );
    }

    #[test]
    fn heartbeat_watch_goes_stale_only_after_silence() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut w = HeartbeatWatch::new();
        assert!(
            !w.observe(Some(1), t0),
            "first sight starts the grace window"
        );
        assert!(
            !w.observe(Some(2), t0 + Duration::from_secs(10)),
            "advancing heartbeat never goes stale"
        );
        assert!(
            !w.observe(Some(2), t0 + Duration::from_secs(11)),
            "1 s of silence is not yet stale"
        );
        assert!(
            w.observe(Some(2), t0 + Duration::from_secs(13)),
            "silent past STALE_AFTER -> stale"
        );
        assert!(
            !w.observe(Some(3), t0 + Duration::from_secs(14)),
            "resumed heartbeat recovers"
        );
    }

    #[test]
    fn heartbeat_watch_treats_unreadable_as_silence() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut w = HeartbeatWatch::new();
        assert!(!w.observe(None, t0), "grace window on first sight");
        assert!(w.observe(None, t0 + Duration::from_secs(3)));
        assert!(
            !w.observe(Some(1), t0 + Duration::from_secs(4)),
            "file back -> recovers"
        );
    }

    /// End-to-end through the real exports: a stale heartbeat bypasses the
    /// chain, and a RESUMED heartbeat (with no new daemon publish) rebuilds
    /// and restores it. Guards the recovery half of the staleness watchdog —
    /// without it, the worker only ever latches `enabled = false` and never
    /// re-reads the snapshot once the daemon comes back.
    #[test]
    fn stale_heartbeat_bypasses_then_recovers_on_beat() {
        let rate = 48_000.0;
        let tone_hz = 1_000.0;
        let max_frames = 1024u32;

        // Publish an audible chain: +12 dB band at 1 kHz.
        let band = ApoFilter::builder()
            .filter_type(FilterType::Peaking)
            .freq(tone_hz)
            .gain_db(12.0)
            .q(4.0)
            .enabled(true)
            .channels(2)
            .sample_rate(rate)
            .build()
            .unwrap();
        let chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(rate)
            .add_filter(band)
            .build();
        let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
        w.publish(&chain);
        w.beat();

        let p = resonance_apo_create();
        assert!(!p.is_null(), "create returned null");
        resonance_apo_lock(p, 2, rate, max_frames);
        // Let the worker build the freshly published chain before driving audio.
        std::thread::sleep(std::time::Duration::from_millis(150));

        let amp = 0.2f64;
        let frames = (rate * 0.25) as usize;
        let w0 = 2.0 * PI * tone_hz / rate;

        // Process a fresh tone through the engine, chunked to `max_frames`
        // (matching the real callback buffer size), and return the measured
        // steady-state gain in dB.
        let measure_gain = || -> f64 {
            let mut buf: Vec<f32> = (0..frames)
                .flat_map(|i| {
                    let s = (amp * (w0 * i as f64).sin()) as f32;
                    [s, s]
                })
                .collect();
            let mut off = 0usize;
            while off < frames {
                let n = (frames - off).min(max_frames as usize);
                resonance_apo_process(p, buf[off * 2..].as_mut_ptr(), n as u32, 2);
                off += n;
            }
            let skip = frames / 4;
            let mono: Vec<f64> = (skip..frames).map(|i| f64::from(buf[i * 2])).collect();
            let in_rms = amp / 2f64.sqrt();
            let out_rms = (mono.iter().map(|x| x * x).sum::<f64>() / mono.len() as f64).sqrt();
            20.0 * (out_rms / in_rms).log10()
        };

        // Phase 1: heartbeat fresh (from the single beat above) → chain
        // built, band audible.
        let gain_live = measure_gain();
        assert!(
            (gain_live - 12.0).abs() < 1.5,
            "expected ~+12 dB while heartbeat is live, got {gain_live:.2}"
        );

        // Phase 2: stop beating; wait past STALE_AFTER (2 s) plus a few
        // worker ticks so the bypass has definitely landed.
        std::thread::sleep(std::time::Duration::from_millis(2200));
        let gain_stale = measure_gain();
        assert!(
            gain_stale.abs() < 1.0,
            "expected ~0 dB (bypassed) once heartbeat goes stale, got {gain_stale:.2}"
        );

        // Phase 3: resume beating — deliberately WITHOUT any new publish —
        // so the only way back is the worker invalidating its latched
        // generation and rebuilding from the still-current snapshot.
        for _ in 0..6 {
            w.beat();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        let gain_recovered = measure_gain();
        assert!(
            (gain_recovered - 12.0).abs() < 1.5,
            "expected ~+12 dB again after heartbeat resumes with no new publish, got {gain_recovered:.2}"
        );

        resonance_apo_unlock(p);
        resonance_apo_destroy(p);

        // Restore a flat default state for the following tests.
        let mut w = ApoStateWriter::create(&default_state_path()).expect("state writer");
        w.publish(
            &ProcessorChain::builder()
                .channels(2)
                .sample_rate(48_000.0)
                .build(),
        );
    }
}
