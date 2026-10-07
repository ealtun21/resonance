//! Live-audio end-to-end test agent: plays deterministic signals through the
//! real OS audio stack with Resonance in the path, records the device output,
//! and checks it against an offline render of the daemon's own chain.
//! See `docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`.
// Never-shipped test crate: error/panic docs and product-name backticks are noise.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown
)]

pub mod checks;
pub mod compare;
pub mod latency;
pub mod ratechain;
pub mod render;
pub mod report;
pub mod scenario;
pub mod stimulus;
