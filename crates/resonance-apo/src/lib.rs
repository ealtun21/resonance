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

// The C-ABI engine + its logging are only ever linked by the Windows C++ APO
// shell; on other platforms the daemon links `state` alone, so don't compile
// the FFI/log modules as dead code there.
#[cfg(target_os = "windows")]
mod ffi;
#[cfg(target_os = "windows")]
mod log;
