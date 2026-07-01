#![no_main]

//! Fuzz the `EqualizerAPO` `.txt` preset parser.
//!
//! APO `.txt` configs are untrusted user input. `parse_apo` must never panic —
//! it must return `Ok(Preset)` or `Err(ApoError)`. Note this transitively
//! exercises the `GraphicEQ:` curve fitter (`graphic::fit_graphic_eq`), so a
//! panic in the fitter surfaces here too. Bytes are decoded lossily so malformed
//! UTF-8 also reaches the parser.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = resonance_preset::apo::parse_apo(&text);
});
