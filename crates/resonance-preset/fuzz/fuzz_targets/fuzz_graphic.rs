#![no_main]

//! Fuzz the `GraphicEQ` curve fitter and summariser.
//!
//! `fit_graphic_eq` runs a Levenberg–Marquardt optimiser over `(freq, gain)`
//! points that ultimately come from an untrusted `GraphicEQ:` line (NaN/Inf,
//! zero/negative/huge frequencies, cancelling pairs, huge point counts). Neither
//! it nor `graphic_eq_summary` may panic on any input.
//!
//! Two paths are exercised from one input:
//!   1. The raw bytes are reinterpreted as a slice of `f64` pairs and fed
//!      straight to `fit_graphic_eq` — the most direct way to reach the
//!      optimiser's numeric edge cases (non-finite, degenerate matrices).
//!   2. The bytes are decoded as text and passed to `graphic_eq_summary`, which
//!      parses a `GraphicEQ:` line without fitting.

use libfuzzer_sys::fuzz_target;

fn bytes_to_points(data: &[u8]) -> Vec<(f64, f64)> {
    data.chunks_exact(16)
        .map(|c| {
            let f = f64::from_le_bytes(c[0..8].try_into().unwrap());
            let g = f64::from_le_bytes(c[8..16].try_into().unwrap());
            (f, g)
        })
        .collect()
}

fuzz_target!(|data: &[u8]| {
    // Path 1: direct numeric fit over arbitrary f64 pairs.
    let points = bytes_to_points(data);
    let _ = resonance_preset::graphic::fit_graphic_eq(&points);

    // Path 2: the textual summary parser.
    let text = String::from_utf8_lossy(data);
    let _ = resonance_preset::graphic::graphic_eq_summary(&text);
});
