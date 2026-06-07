//! Resonance APO engine.
//!
//! The Windows audio engine instantiates system-effect APOs via COM
//! *aggregation*, which a windows-rs `#[implement]` object cannot satisfy. So
//! the COM/aggregation boilerplate lives in a thin C++ shell built on the SDK's
//! `CBaseAudioProcessingObject` (see `cpp/resonance_apo.cpp`), and this crate
//! provides the actual DSP + daemon control-bridge as a C ABI ([`ffi`]) that the
//! shell links and calls. All signal processing stays in Rust (`resonance-dsp`).
//!
//! The [`state`] module (the memory-mapped daemon→APO bridge) is platform
//! agnostic; the daemon links it on every OS.

pub mod state;

mod ffi;
mod log;
