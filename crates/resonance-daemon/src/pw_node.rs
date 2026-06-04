use crate::state::AudioCommand;
use anyhow::{Context, Result};
use pipewire::{self as pw, stream::StreamFlags};
use resonance_dsp::chain::ProcessorChain;
use rtrb::RingBuffer;
use spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use spa::pod::{Object, Pod, Value};
use std::thread::{self, JoinHandle};
use tracing::info;

pub const CHANNELS: usize = 2;
pub const SAMPLE_RATE: u32 = 48000;
const RING_FRAMES: usize = 16384; // ~340 ms headroom
pub const SPECTRUM_BUF: usize = 8192; // ring buffer for spectrum computation

/// Spawn the PipeWire node on a dedicated thread.
/// Returns a JoinHandle and two ring-buffer producers:
///   - `cmd_rx`: audio thread receives commands from IPC thread
///   - `spectrum_rx`: spectrum task drains processed samples
pub fn spawn(
    cmd_rx: rtrb::Consumer<AudioCommand>,
    spectrum_tx: rtrb::Producer<f32>,
    initial_chain: ProcessorChain,
) -> Result<JoinHandle<()>> {
    let handle = thread::Builder::new()
        .name("resonance-pw".into())
        .spawn(move || {
            if let Err(e) = run_loop(cmd_rx, spectrum_tx, initial_chain) {
                tracing::error!("PipeWire thread error: {e:#}");
            }
        })?;
    Ok(handle)
}

fn run_loop(
    mut cmd_rx: rtrb::Consumer<AudioCommand>,
    mut spectrum_tx: rtrb::Producer<f32>,
    mut chain: ProcessorChain,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None).context("create PW mainloop")?;
    let context =
        pw::context::ContextBox::new(mainloop.loop_(), None).context("create PW context")?;
    let core = context
        .connect(None)
        .context("connect to PipeWire daemon")?;

    let (mut audio_tx, mut audio_rx) = RingBuffer::<f32>::new(RING_FRAMES * CHANNELS);

    // ── Capture stream ─────────────────────────────────────────────────────
    // Appears as "Resonance EQ" in sound settings — applications can route to it.
    let capture = pw::stream::StreamBox::new(
        &core,
        "Resonance EQ",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => "resonance",
            *pw::keys::NODE_DESCRIPTION => "Resonance EQ",
            *pw::keys::NODE_GROUP => "resonance-filter",
        },
    )
    .context("create capture stream")?;

    let cap_params = build_f32_params(SAMPLE_RATE, CHANNELS as u32);
    let mut cap_param_pod = [Pod::from_bytes(&cap_params).unwrap()];

    let _cap_listener = capture
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buf) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buf.datas_mut();
            let Some(data) = datas[0].data() else {
                return;
            };
            let samples = cast_u8_to_f32(data);
            let to_write = samples.len().min(audio_tx.slots());
            for &s in samples.iter().take(to_write) {
                let _ = audio_tx.push(s);
            }
        })
        .register()
        .context("register capture listener")?;

    capture
        .connect(
            spa::utils::Direction::Input,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut cap_param_pod,
        )
        .context("connect capture stream")?;

    // ── Playback stream ────────────────────────────────────────────────────
    // Outputs processed audio; WirePlumber routes this to the default sink.
    let playback = pw::stream::StreamBox::new(
        &core,
        "Resonance EQ Output",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Source/Virtual",
            *pw::keys::NODE_NAME => "resonance-out",
            *pw::keys::NODE_DESCRIPTION => "Resonance EQ Output",
            *pw::keys::NODE_GROUP => "resonance-filter",
        },
    )
    .context("create playback stream")?;

    let play_params = build_f32_params(SAMPLE_RATE, CHANNELS as u32);
    let mut play_param_pod = [Pod::from_bytes(&play_params).unwrap()];

    let _play_listener = playback
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            while let Ok(cmd) = cmd_rx.pop() {
                apply_command(&mut chain, cmd);
            }

            let Some(mut buf) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buf.datas_mut();
            let data = &mut datas[0];

            let n_frames = {
                let Some(slice) = data.data() else {
                    return;
                };
                let n_f32 = slice.len() / 4;
                let n_frames = n_f32 / CHANNELS;

                let available = audio_rx.slots();
                let readable = available.min(n_f32);
                let mut f64_buf = vec![0.0f64; n_f32];

                for dst in f64_buf[..readable].iter_mut() {
                    if let Ok(s) = audio_rx.pop() {
                        *dst = s as f64;
                    }
                }

                if chain.enabled {
                    chain.process(&mut f64_buf);
                }

                let out_f32 = cast_u8_to_f32_mut(slice);
                for (dst, &src) in out_f32.iter_mut().zip(f64_buf.iter()) {
                    *dst = src as f32;
                }

                // Feed spectrum ring buffer (post-DSP, mono mix)
                let spec_slots = spectrum_tx.slots();
                let spec_frames = (n_frames / 2).min(spec_slots);
                for i in 0..spec_frames {
                    let l = f64_buf[i * CHANNELS] as f32;
                    let r = f64_buf[i * CHANNELS + 1] as f32;
                    let _ = spectrum_tx.push((l + r) * 0.5);
                }

                n_frames
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = (4 * CHANNELS) as _;
            *chunk.size_mut() = (4 * CHANNELS * n_frames) as _;
        })
        .register()
        .context("register playback listener")?;

    playback
        .connect(
            spa::utils::Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut play_param_pod,
        )
        .context("connect playback stream")?;

    info!(
        "PipeWire streams ready ({}ch @ {} Hz)  —  set app output to 'Resonance EQ'",
        CHANNELS, SAMPLE_RATE
    );

    mainloop.run();
    Ok(())
}

fn apply_command(chain: &mut ProcessorChain, cmd: AudioCommand) {
    match cmd {
        AudioCommand::SetPower(on) => chain.enabled = on,
        AudioCommand::SetPreamp(db) => chain.preamp_db = db,
        AudioCommand::SetEffectIntensity { effect, value } => {
            chain.set_effect_intensity(effect, value);
        }
        AudioCommand::SetEffectEnabled { effect, on } => {
            chain.set_effect_enabled(effect, on);
        }
        AudioCommand::ReplaceChain(new_chain) => *chain = *new_chain,
        AudioCommand::Reset => chain.reset(),
    }
}

fn build_f32_params(rate: u32, channels: u32) -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(channels);
    let mut position = [0u32; MAX_CHANNELS];
    position[0] = spa_sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa_sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);

    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(Object {
            type_: spa_sys::SPA_TYPE_OBJECT_Format,
            id: spa_sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner()
}

fn cast_u8_to_f32(data: &[u8]) -> &[f32] {
    let len = data.len() / 4;
    let ptr = data.as_ptr() as *const f32;
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn cast_u8_to_f32_mut(data: &mut [u8]) -> &mut [f32] {
    let len = data.len() / 4;
    let ptr = data.as_mut_ptr() as *mut f32;
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}
