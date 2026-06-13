use crate::state::{SPECTRUM_BINS, SharedState};
use rustfft::{FftPlanner, num_complex::Complex};
use std::time::Duration;

const FFT_SIZE: usize = 4096;
const BINS: usize = SPECTRUM_BINS;
const FREQ_MIN: f64 = 25.0;
const FREQ_MAX: f64 = 20000.0;

/// Drains the spectrum ring buffer periodically, computes FFT-based band energies,
/// and publishes normalised bins to SharedState.
pub async fn run(mut rx: rtrb::Consumer<f32>, state: SharedState) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| {
            0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / FFT_SIZE as f64).cos()) as f32
        })
        .collect();

    // Log-spaced bin edges (BINS+1 boundaries from FREQ_MIN to FREQ_MAX).
    let edges: Vec<f64> = (0..=BINS)
        .map(|i| {
            let t = i as f64 / BINS as f64;
            FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
        })
        .collect();

    let mut buf: Vec<f32> = vec![0.0; FFT_SIZE];
    let mut write_pos: usize = 0;

    let mut envelope = [0.0f32; BINS];
    const ATTACK: f32 = 0.0; // instant — track peaks immediately
    const RELEASE: f32 = 0.30; // snappier fall (less smear)

    // Reused across iterations so the 40 Hz loop never allocates.
    let mut fft_buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT_SIZE];

    let mut interval = tokio::time::interval(Duration::from_millis(25)); // ~40 fps

    loop {
        interval.tick().await;

        // Always drain the ring so it can't back up, even while idle.
        let available = rx.slots();
        for _ in 0..available {
            if let Ok(s) = rx.pop() {
                buf[write_pos % FFT_SIZE] = s;
                write_pos += 1;
            }
        }

        if write_pos < FFT_SIZE {
            continue;
        }

        // Skip the FFT entirely when no client has polled recently: the daemon
        // runs continuously but the spectrum is only consumed while a TUI/GUI is
        // open. Drain (above) keeps the ring healthy; we just don't spend cycles.
        // Use the live chain rate so bins are labelled correctly at 44.1k etc.
        // (was hardcoded 48k → ~9% bin-frequency error on a 44.1k device).
        let (sr, watching) = {
            let inner = state.0.lock().unwrap();
            let watching = inner
                .last_poll
                .map(|t| t.elapsed() < Duration::from_millis(1500))
                .unwrap_or(false);
            let r = inner.chain.sample_rate;
            (if r.is_finite() && r > 0.0 { r } else { 48000.0 }, watching)
        };
        if !watching {
            continue;
        }

        let start = write_pos % FFT_SIZE;
        for (i, slot) in fft_buf.iter_mut().enumerate() {
            let sample_idx = (start + i) % FFT_SIZE;
            *slot = Complex::new(buf[sample_idx] * window[i], 0.0);
        }

        fft.process(&mut fft_buf);

        let hz_per_bin = sr / FFT_SIZE as f64;
        let norm = 2.0 / FFT_SIZE as f32;

        let mut bins = [0.0f32; BINS];
        for (k, c) in fft_buf[..FFT_SIZE / 2].iter().enumerate() {
            let freq = k as f64 * hz_per_bin;
            if freq < edges[0] {
                continue;
            }
            let mag = c.norm() * norm;
            let bin_idx = band_for_freq(freq, &edges);
            if bin_idx < BINS && bins[bin_idx] < mag {
                bins[bin_idx] = mag;
            }
        }

        // Map to display: dBFS [-66, 0] → [0, 1]
        for (i, &raw) in bins.iter().enumerate() {
            let db = 20.0 * raw.max(1e-6).log10();
            let normalised = ((db + 66.0) / 66.0).clamp(0.0, 1.0);
            if normalised > envelope[i] {
                envelope[i] = ATTACK * envelope[i] + (1.0 - ATTACK) * normalised;
            } else {
                envelope[i] *= 1.0 - RELEASE;
            }
        }

        state.update_spectrum(envelope);
    }
}

fn band_for_freq(freq: f64, edges: &[f64]) -> usize {
    // edges has BINS+1 entries; find the band whose [lo, hi) contains freq.
    for i in 0..BINS {
        if freq < edges[i + 1] {
            return i;
        }
    }
    BINS - 1
}
