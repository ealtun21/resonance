#![no_main]

//! Fuzz the FxSound `.fac` preset parser.
//!
//! `.fac` files are untrusted user input. The parser must never panic on any
//! byte sequence — it must return `Ok(Preset)` or `Err(FacError)`. Arbitrary
//! bytes are decoded lossily so malformed UTF-8 also reaches the parser (which
//! takes `&str`); we only assert the absence of a panic.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = resonance_preset::fac::parse_fac(&text);
});
