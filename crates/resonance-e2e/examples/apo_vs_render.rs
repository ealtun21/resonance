//! Windows-only diagnostic: run the APO engine (the exact object audiodg calls) on a
//! stimulus and compare with the offline render of the same exported chain.
#![allow(clippy::doc_markdown)]
#[cfg(windows)]
mod imp {
    use resonance_e2e::render::{load_exported_chain, render};
    use resonance_e2e::stimulus::generate;
    use std::path::PathBuf;

    extern crate resonance_apo;

    unsafe extern "C" {
        fn resonance_apo_create() -> *mut core::ffi::c_void;
        fn resonance_apo_lock(p: *mut core::ffi::c_void, channels: u32, rate: f64, max_frames: u32);
        fn resonance_apo_process(
            p: *mut core::ffi::c_void,
            buf: *mut f32,
            frames: u32,
            channels: u32,
        );
    }

    pub fn main() {
        let chain = PathBuf::from(std::env::args().nth(1).expect("chain.bin"));
        let block: usize = std::env::args().nth(2).map_or(480, |s| s.parse().unwrap());
        let state = resonance_apo::state::default_state_path();
        std::fs::copy(&chain, &state).unwrap();
        std::fs::copy(
            resonance_apo::state::ir_path_for(&chain),
            resonance_apo::state::ir_path_for(&state),
        )
        .ok();
        let stim = generate(48_000, 2, 3.0);
        let (mut c, _) = load_exported_chain(&chain, 2, 48_000.0).unwrap();
        let rb: usize = std::env::var("RENDER_BLOCK").map_or(1024, |s| s.parse().unwrap());
        let expected = render(&mut c, &stim.samples, rb);
        let mut got = stim.samples.clone();
        unsafe {
            let p = resonance_apo_create();
            resonance_apo_lock(p, 2, 48_000.0, block as u32);
            let wait: u64 = std::env::args().nth(3).map_or(0, |s| s.parse().unwrap());
            std::thread::sleep(std::time::Duration::from_millis(wait));
            for b in got.chunks_mut(block * 2) {
                resonance_apo_process(p, b.as_mut_ptr(), (b.len() / 2) as u32, 2);
            }
        }
        let (mut e2, mut en, mut dot) = (0f64, 0f64, 0f64);
        for (a, b) in got.iter().zip(&expected) {
            e2 += f64::from(a - b).powi(2);
            en += f64::from(*b).powi(2);
            dot += f64::from(*a) * f64::from(*b);
        }
        println!(
            "block {block}: rel diff {:.3e}, gain {:.4}, equal {}",
            (e2 / en).sqrt(),
            dot / en,
            got == expected
        );
    }
}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("apo_vs_render runs on Windows only");
}
