use crate::state::{SPECTRUM_BINS, SharedState};
use rustfft::{FftPlanner, num_complex::Complex};
use std::time::Duration;

const FFT_SIZE: usize = 4096;
const BINS: usize = SPECTRUM_BINS;
// Matches the clients' log frequency axis (`resonance_ipc::fr::LOG_MIN`/`LOG_MAX`
// = log10(20)..log10(20000)) so display band `i` lands under the axis position
// the GUI/TUI draw it at.
const FREQ_MIN: f64 = 20.0;
const FREQ_MAX: f64 = 20000.0;

/// Display-envelope state machine for the analyzer bars, factored out of the
/// async [`run`] loop so its anti-freeze behaviour is unit-testable. Holds the
/// smoothed per-bin envelope plus the ring-starvation counter.
struct SpectrumEnvelope {
    bins: [f32; BINS],
    /// Consecutive ticks the ring delivered no new samples — i.e. the RT audio
    /// callback stopped feeding us (device removed, graph suspended, daemon
    /// glitch).
    starved_ticks: u32,
}

impl SpectrumEnvelope {
    const ATTACK: f32 = 0.0; // instant — track peaks immediately
    const RELEASE: f32 = 0.30; // snappier fall (less smear)
    /// Ticks of silence before the ring counts as starved and we decay rather
    /// than re-FFT a frozen window. ~400 ms at the 25 ms (40 fps) tick — well
    /// past the largest expected quantum gap (~170 ms), so ordinary bursty
    /// delivery never trips it.
    const STARVE_TICKS: u32 = 16;

    fn new() -> Self {
        Self {
            bins: [0.0; BINS],
            starved_ticks: 0,
        }
    }

    /// Record whether the ring delivered new samples since the last tick.
    fn note_feed(&mut self, got_new: bool) {
        if got_new {
            self.starved_ticks = 0;
        } else {
            self.starved_ticks = self.starved_ticks.saturating_add(1);
        }
    }

    /// The ring has been silent long enough that re-FFTing it would just hold a
    /// constant (frozen) spectrum.
    fn starved(&self) -> bool {
        self.starved_ticks >= Self::STARVE_TICKS
    }

    fn lit(&self) -> bool {
        self.bins.iter().any(|&e| e > 0.0)
    }

    /// No client is watching: clear the cached spectrum so a client that
    /// reconnects starts from silence rather than the frozen last frame. Returns
    /// the frame to publish, or `None` if it was already silent (so the caller
    /// can skip taking the state lock every tick).
    fn idle(&mut self) -> Option<[f32; BINS]> {
        if self.lit() {
            self.bins = [0.0; BINS];
            Some(self.bins)
        } else {
            None
        }
    }

    /// Ring starved while a client is watching: glide toward silence instead of
    /// freezing on the last frame. Returns the frame to publish, or `None` once
    /// fully silent.
    fn decay(&mut self) -> Option<[f32; BINS]> {
        if self.lit() {
            for e in &mut self.bins {
                *e *= 1.0 - Self::RELEASE;
                if *e < 1e-4 {
                    *e = 0.0;
                }
            }
            Some(self.bins)
        } else {
            None
        }
    }

    /// Fresh FFT magnitudes (peak-per-band, linear): map to the dBFS display
    /// range, snap up to peaks and glide down. Returns the frame to publish.
    fn apply(&mut self, mags: &[f32; BINS]) -> [f32; BINS] {
        for (e, &raw) in self.bins.iter_mut().zip(mags.iter()) {
            // dBFS [-66, 0] → [0, 1]
            let db = 20.0 * raw.max(1e-6).log10();
            let normalised = ((db + 66.0) / 66.0).clamp(0.0, 1.0);
            if normalised > *e {
                *e = Self::ATTACK * *e + (1.0 - Self::ATTACK) * normalised;
            } else {
                *e *= 1.0 - Self::RELEASE;
            }
        }
        self.bins
    }
}

/// Drains the spectrum ring buffer periodically, computes FFT-based band energies,
/// and publishes normalised bins to `SharedState`.
pub async fn run(mut rx: rtrb::Consumer<f32>, state: SharedState) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| {
            0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / FFT_SIZE as f64).cos()) as f32
        })
        .collect();

    // Log-spaced bin edges (BINS+1 boundaries from FREQ_MIN to FREQ_MAX).
    let edges = band_edges();

    let mut buf: Vec<f32> = vec![0.0; FFT_SIZE];
    let mut write_pos: usize = 0;
    let mut env = SpectrumEnvelope::new();

    // Reused across iterations so the 40 Hz loop never allocates.
    let mut fft_buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut half_mags: Vec<f32> = vec![0.0; FFT_SIZE / 2];
    let mut drained: Vec<f32> = Vec::with_capacity(8192);

    let mut interval = tokio::time::interval(Duration::from_millis(25)); // ~40 fps

    loop {
        interval.tick().await;

        // Always drain the ring so it can't back up, even while idle.
        let available = rx.slots();
        drained.clear();
        for _ in 0..available {
            if let Ok(s) = rx.pop() {
                buf[write_pos % FFT_SIZE] = s;
                write_pos += 1;
                drained.push(s);
            }
        }
        env.note_feed(available > 0);

        // Mirror the fresh samples into the rolling capture buffer that backs
        // `CaptureOutput` (the `resonance verify` harness). One lock per tick.
        if !drained.is_empty() {
            let mut inner = state.0.lock().unwrap();
            let cap = &mut inner.capture;
            for &s in &drained {
                if cap.len() == crate::state::CAPTURE_BUF {
                    cap.pop_front();
                }
                cap.push_back(s);
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
                .is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
            let r = inner.chain.sample_rate;
            (if r.is_finite() && r > 0.0 { r } else { 48000.0 }, watching)
        };

        // No client watching → clear the cache once; ring starved while watched
        // → decay toward silence rather than re-FFTing the frozen buffer. Both
        // publish only on a change, so an idle/silent daemon never spins the lock.
        if !watching {
            if let Some(frame) = env.idle() {
                state.update_spectrum(frame);
            }
            continue;
        }
        if env.starved() {
            if let Some(frame) = env.decay() {
                state.update_spectrum(frame);
            }
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

        for (m, c) in half_mags.iter_mut().zip(fft_buf[..FFT_SIZE / 2].iter()) {
            *m = c.norm() * norm;
        }
        let mags = fold_to_bands(&half_mags, hz_per_bin, &edges);

        state.update_spectrum(env.apply(&mags));
    }
}

/// The BINS+1 log-spaced band boundaries from [`FREQ_MIN`] to [`FREQ_MAX`].
fn band_edges() -> Vec<f64> {
    (0..=BINS)
        .map(|i| {
            let t = i as f64 / BINS as f64;
            FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(t)
        })
        .collect()
}

/// Fold the FFT half-spectrum (`mags[k]` = linear magnitude of bin `k`, whose
/// centre frequency is `k * hz_per_bin`) into the [`BINS`] log-spaced display
/// bands defined by `edges`.
///
/// For each band we take the peak of every FFT bin whose centre lands in its
/// `[lo, hi)` range. At the low end the log-spaced bands are only a few Hz wide
/// — narrower than the FFT resolution (`hz_per_bin`) — so a band can contain no
/// bin centre at all. Rather than leave it permanently dark (the dead-bar bug),
/// we sample the spectrum by linear interpolation at the band's geometric
/// centre, giving a smooth low-frequency analyzer that tracks whatever plays.
fn fold_to_bands(mags: &[f32], hz_per_bin: f64, edges: &[f64]) -> [f32; BINS] {
    let mut out = [0.0f32; BINS];
    if mags.is_empty() || !hz_per_bin.is_finite() || hz_per_bin <= 0.0 {
        return out;
    }
    for (b, slot) in out.iter_mut().enumerate() {
        let (lo, hi) = (edges[b], edges[b + 1]);
        // FFT bins whose centre falls in [lo, hi): k in [ceil(lo/step), ceil(hi/step)).
        // ceil(lo/step) >= 1 here (lo >= FREQ_MIN >= step at these rates) so DC is
        // never included.
        let k_start = (lo / hz_per_bin).ceil() as usize;
        let k_end = ((hi / hz_per_bin).ceil() as usize).min(mags.len());
        *slot = if k_start < k_end {
            mags[k_start..k_end].iter().copied().fold(0.0, f32::max)
        } else {
            interp_mag(mags, (lo * hi).sqrt() / hz_per_bin)
        };
    }
    out
}

/// Linear interpolation of the FFT magnitude at fractional bin position `pos`.
fn interp_mag(mags: &[f32], pos: f64) -> f32 {
    let pos = pos.max(0.0);
    let k = pos.floor() as usize;
    match (mags.get(k), mags.get(k + 1)) {
        (Some(&a), Some(&b)) => {
            let frac = (pos - k as f64) as f32;
            a * (1.0 - frac) + b * frac
        }
        (Some(&a), None) => a,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_band_lights_for_broadband_input() {
        // A flat, broadband signal excites every frequency, so every display
        // band must light. Regression for the low-frequency dead-bar bug: the
        // log-spaced bands below ~150 Hz are narrower than the FFT resolution
        // (sr / FFT_SIZE), so the old scatter mapping — assign each FFT bin to
        // one band — left several low bands with no bin at all, permanently
        // dark regardless of what was playing. The effect worsens with sample
        // rate (coarser hz_per_bin), so exercise the common rates.
        let edges = band_edges();
        let mags = vec![1.0f32; FFT_SIZE / 2]; // equal energy in every FFT bin
        for &sr in &[44100.0f64, 48000.0, 96000.0] {
            let hz_per_bin = sr / FFT_SIZE as f64;
            let bands = fold_to_bands(&mags, hz_per_bin, &edges);
            let dark: Vec<usize> = bands
                .iter()
                .enumerate()
                .filter(|&(_, &v)| v <= 0.0)
                .map(|(i, _)| i)
                .collect();
            assert!(
                dark.is_empty(),
                "broadband input left dark bands at {sr} Hz: {dark:?}"
            );
        }
    }

    #[test]
    fn a_tone_peaks_in_the_band_that_spans_it() {
        // Lighting every band must not come at the cost of frequency accuracy:
        // a single-bin tone must still peak in the display band whose [lo, hi)
        // range contains it — including low tones that rely on interpolation.
        let edges = band_edges();
        let sr = 48000.0f64;
        let hz_per_bin = sr / FFT_SIZE as f64;
        for &k in &[4usize, 40, 400] {
            // ~47 Hz (interpolated region), ~469 Hz, ~4688 Hz (gathered region)
            let mut mags = vec![0.0f32; FFT_SIZE / 2];
            mags[k] = 1.0;
            let tone = k as f64 * hz_per_bin;
            let bands = fold_to_bands(&mags, hz_per_bin, &edges);
            let peak_b = bands
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            assert!(
                edges[peak_b] <= tone && tone < edges[peak_b + 1],
                "tone {tone:.1} Hz peaked in band {peak_b} = [{:.1}, {:.1})",
                edges[peak_b],
                edges[peak_b + 1]
            );
        }
    }

    #[test]
    fn steady_audio_keeps_the_bars_lit() {
        // A constant non-zero signal must keep the analyzer lit: it tracks live
        // audio and must NOT decay to silence on its own.
        let mut env = SpectrumEnvelope::new();
        let mags = [0.1f32; BINS]; // ~ -20 dBFS in every band
        let mut frame = [0.0f32; BINS];
        for _ in 0..200 {
            env.note_feed(true); // ring keeps delivering
            assert!(!env.starved());
            frame = env.apply(&mags);
        }
        assert!(
            frame.iter().all(|&e| e > 0.4),
            "steady audio must hold the bars near the signal level, not fade out"
        );
    }

    #[test]
    fn ring_starvation_decays_to_silence() {
        // Regression for the freeze bug: once the ring stops being fed (power-off
        // with the old code, or the RT callback stalling), the bars must glide to
        // zero instead of holding the last frame forever.
        let mut env = SpectrumEnvelope::new();
        let mags = [0.5f32; BINS];
        for _ in 0..50 {
            env.note_feed(true);
            env.apply(&mags);
        }
        assert!(env.lit(), "precondition: the spectrum is lit");

        // Stop feeding the ring.
        for _ in 0..SpectrumEnvelope::STARVE_TICKS {
            env.note_feed(false);
        }
        assert!(
            env.starved(),
            "sustained silence must register as starvation"
        );

        let mut ticks = 0;
        while env.decay().is_some() {
            ticks += 1;
            assert!(ticks < 1000, "decay must terminate at silence");
        }
        assert!(!env.lit(), "a starved spectrum must reach exact silence");
    }

    #[test]
    fn brief_gaps_do_not_trip_starvation() {
        // A large audio quantum delivers in bursts: a few empty ticks between
        // bursts must NOT be mistaken for a stalled ring.
        let mut env = SpectrumEnvelope::new();
        for _ in 0..(SpectrumEnvelope::STARVE_TICKS - 1) {
            env.note_feed(false);
            assert!(
                !env.starved(),
                "a gap shorter than the debounce is not starvation"
            );
        }
        env.note_feed(true); // next burst arrives
        assert!(!env.starved());
        assert_eq!(env.starved_ticks, 0, "a fresh burst resets the counter");
    }

    #[test]
    fn idle_clears_once_then_stays_quiet() {
        // With no client watching, idle() emits one silenced frame and then
        // reports nothing — so the run loop stops taking the state lock.
        let mut env = SpectrumEnvelope::new();
        env.apply(&[0.3f32; BINS]);
        assert!(env.lit());

        let cleared = env.idle().expect("first idle publishes the silenced frame");
        assert!(cleared.iter().all(|&e| e <= 0.0), "the frame is silenced");
        assert!(!env.lit());
        assert!(env.idle().is_none(), "already silent → nothing to publish");
    }
}
