//! Live-audio end-to-end test agent: plays deterministic signals through the
//! real OS audio stack with Resonance in the path, records the device output,
//! and checks it against an offline render of the daemon's own chain.
//! See `docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`.
// Never-shipped test crate: error/panic docs and product-name backticks are noise.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::must_use_candidate
)]

pub mod checks;
pub mod common;
pub mod compare;
pub mod latency;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod macos;
#[cfg(any(windows, target_os = "macos", feature = "cross-check"))]
pub mod native;
pub mod ratechain;
pub mod render;
pub mod report;
#[cfg(target_os = "linux")]
pub mod runner;
pub mod scenario;
pub mod stimulus;
pub mod windows;
