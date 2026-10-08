//! macOS/Windows harness self-check: play the stimulus into `<play device>` and record
//! `<record device>` (an input device) with no Resonance in the path; the recording must
//! be bit-exact. usage: loopback_check <play> <record> <channels> <rate>
#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use cpal::traits::{DeviceTrait, HostTrait};
    use resonance_e2e::common::MAX_LAG_SECS;
    use resonance_e2e::compare::{CompareMode, compare};
    use resonance_e2e::native::play_and_record;
    use resonance_e2e::stimulus::generate;

    fn find(name: &str, input: bool) -> cpal::Device {
        let host = cpal::default_host();
        let mut it: Box<dyn Iterator<Item = cpal::Device>> = if input {
            Box::new(host.input_devices().unwrap())
        } else {
            Box::new(host.output_devices().unwrap())
        };
        it.find(|d| d.description().is_ok_and(|x| x.name() == name))
            .unwrap_or_else(|| panic!("no device {name}"))
    }

    pub fn main() {
        let a: Vec<String> = std::env::args().collect();
        let (play, rec) = (&a[1], &a[2]);
        let ch: usize = a[3].parse().unwrap();
        let rate: u32 = a[4].parse().unwrap();
        let stim = generate(rate, ch, 3.0);
        let r = play_and_record(
            &find(play, false),
            &find(rec, true),
            true,
            &stim.samples,
            ch,
            rate,
            (MAX_LAG_SECS * f64::from(rate)) as usize,
        )
        .unwrap();
        let o = compare(
            &stim.samples,
            &r.samples,
            ch,
            stim.body.clone(),
            (MAX_LAG_SECS * f64::from(rate)) as usize,
        );
        println!("{} ({} errors)", o.describe(), r.discontinuities);
        std::process::exit(i32::from(!o.passes(CompareMode::Exact)));
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    imp::main();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("loopback_check runs on Windows and macOS only");
}
