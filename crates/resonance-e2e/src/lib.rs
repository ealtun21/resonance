//! Live-audio end-to-end test agent: plays deterministic signals through the
//! real OS audio stack with Resonance in the path, records the device output,
//! and checks it against an offline render of the daemon's own chain.
//! See `docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md`.

pub mod compare;
pub mod stimulus;
