//! WAV impulse-response loading for the convolution stage.
//!
//! Decodes a user-supplied `.wav` (PCM 8/16/24/32-bit int or 32-bit float) into
//! the DSP crate's [`IrData`]; the `ConvolutionEngine` then resamples it to the
//! live DSP rate and prepares the partitioned kernel. Files come from users
//! (room-correction exports, HRTF sets), so every failure is a clean error
//! string for the IPC reply, never a panic.

use resonance_dsp::convolution::IrData;

/// Read + decode a WAV impulse response. Returns the de-interleaved samples at
/// the file's native rate; the display name is the file stem.
pub fn load_wav_ir(path: &str) -> Result<IrData, String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("open '{path}': {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels);
    if channels == 0 {
        return Err(format!("'{path}': zero-channel WAV"));
    }
    if channels > resonance_dsp::channel::MAX_CHANNELS {
        return Err(format!(
            "'{path}': {channels} channels exceeds the supported maximum ({})",
            resonance_dsp::channel::MAX_CHANNELS
        ));
    }

    let interleaved: Vec<f64> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .into_samples::<f32>()
            .map(|s| s.map(f64::from))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("decode '{path}': {e}"))?,
        (hound::SampleFormat::Int, bits @ 1..=32) => {
            // Full-scale for an n-bit signed sample is 2^(n-1).
            let scale = 1.0 / f64::from(1u32 << (bits - 1));
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| f64::from(v) * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| format!("decode '{path}': {e}"))?
        }
        (fmt, bits) => {
            return Err(format!(
                "'{path}': unsupported WAV format ({bits}-bit {fmt:?})"
            ));
        }
    };

    let name = std::path::Path::new(path).file_stem().map_or_else(
        || "impulse".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );

    IrData::from_interleaved(
        name,
        path.to_string(),
        f64::from(spec.sample_rate),
        channels,
        &interleaved,
    )
    .ok_or_else(|| format!("'{path}': no audio samples"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &std::path::Path, spec: hound::WavSpec, frames: &[[f32; 2]]) {
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for f in frames {
            for &s in f {
                match spec.sample_format {
                    hound::SampleFormat::Float => w.write_sample(s).unwrap(),
                    hound::SampleFormat::Int => {
                        let full = f64::from(1u32 << (spec.bits_per_sample - 1));
                        w.write_sample((f64::from(s) * full) as i32).unwrap();
                    }
                }
            }
        }
        w.finalize().unwrap();
    }

    #[test]
    fn loads_float32_stereo_wav() {
        let dir = std::env::temp_dir().join("resonance-ir-test-f32");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ir.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        write_wav(&path, spec, &[[1.0, 0.0], [0.5, -0.5], [0.0, 0.25]]);

        let ir = load_wav_ir(path.to_str().unwrap()).unwrap();
        assert_eq!(ir.channels.len(), 2);
        assert_eq!(ir.frames(), 3);
        assert!((ir.sample_rate - 44_100.0).abs() < 1e-9);
        assert_eq!(ir.name, "ir");
        assert!((ir.channels[0][0] - 1.0).abs() < 1e-6);
        assert!((ir.channels[1][1] + 0.5).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_int16_wav_scaled_to_unit_range() {
        let dir = std::env::temp_dir().join("resonance-ir-test-i16");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ir16.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &[[0.5, -0.5], [0.25, 0.0]]);

        let ir = load_wav_ir(path.to_str().unwrap()).unwrap();
        assert!((ir.channels[0][0] - 0.5).abs() < 1e-3);
        assert!((ir.channels[1][0] + 0.5).abs() < 1e-3);
        assert!((ir.channels[0][1] - 0.25).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_a_clean_error() {
        let err = load_wav_ir("/nonexistent/definitely/missing.wav").unwrap_err();
        assert!(
            err.contains("missing.wav"),
            "error should name the file: {err}"
        );
    }
}
