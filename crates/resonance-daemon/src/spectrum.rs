use crate::state::SharedState;
use rustfft::{FftPlanner, num_complex::Complex};
use std::time::Duration;

const FFT_SIZE: usize = 2048;
const BINS: usize = 16;

// Log-spaced bin boundaries (Hz) — 16 bands covering 20–20000 Hz.
// Each pair defines [lo, hi) for that bin.
const BAND_CENTERS: [f64; BINS] = [
    25.0, 40.0, 63.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1000.0, 1600.0, 2500.0, 4000.0, 6300.0,
    10000.0, 16000.0, 20000.0,
];

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

    let mut buf: Vec<f32> = vec![0.0; FFT_SIZE];
    let mut write_pos: usize = 0;

    // Fast-attack / slow-release envelope per bin
    let mut envelope = [0.0f32; BINS];
    const ATTACK: f32 = 0.8;
    const RELEASE: f32 = 0.05;

    let mut interval = tokio::time::interval(Duration::from_millis(50));

    loop {
        interval.tick().await;

        // Drain whatever is in the ring buffer
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

        // Build windowed FFT input starting from oldest sample
        let start = write_pos % FFT_SIZE;
        let mut fft_buf: Vec<Complex<f32>> = (0..FFT_SIZE)
            .map(|i| {
                let sample_idx = (start + i) % FFT_SIZE;
                Complex::new(buf[sample_idx] * window[i], 0.0)
            })
            .collect();

        fft.process(&mut fft_buf);

        // Compute magnitude per FFT bin
        let sr = 48000.0f64;
        let hz_per_bin = sr / FFT_SIZE as f64;

        let mut bins = [0.0f32; BINS];
        for (k, mag_sq) in fft_buf[..FFT_SIZE / 2]
            .iter()
            .map(|c| c.norm_sqr())
            .enumerate()
        {
            let freq = k as f64 * hz_per_bin;
            if freq < 20.0 {
                continue;
            }
            // Find which display bin this FFT bin belongs to
            let bin_idx = band_for_freq(freq);
            if bins[bin_idx] < mag_sq.sqrt() {
                bins[bin_idx] = mag_sq.sqrt();
            }
        }

        // Peak-normalise across all bins
        let peak: f32 = bins.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
        let scale = 1.0 / peak;

        // Apply AR envelope
        for (i, &raw) in bins.iter().enumerate() {
            let normalised = (raw * scale).min(1.0);
            if normalised > envelope[i] {
                envelope[i] = ATTACK * envelope[i] + (1.0 - ATTACK) * normalised;
            } else {
                envelope[i] *= 1.0 - RELEASE;
            }
        }

        state.update_spectrum(envelope);
    }
}

fn band_for_freq(freq: f64) -> usize {
    for (i, &center) in BAND_CENTERS.iter().enumerate().rev() {
        if freq >= center {
            return i;
        }
    }
    0
}
