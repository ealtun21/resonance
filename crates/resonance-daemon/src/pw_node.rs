use crate::state::AudioCommand;
use anyhow::{Context, Result};
use pipewire::{self as pw, stream::StreamFlags};
use resonance_dsp::chain::ProcessorChain;
use rtrb::RingBuffer;
use spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use spa::pod::{Object, Pod, Value};
use std::thread::{self, JoinHandle};
use tracing::info;

const CHANNELS: usize = 2;
const SAMPLE_RATE: u32 = 48000;
const RING_FRAMES: usize = 16384; // ~340 ms headroom

/// Spawn the PipeWire filter node on a dedicated thread.
/// Returns a handle and a command channel producer for sending parameter updates.
pub fn spawn(
    cmd_rx: rtrb::Consumer<AudioCommand>,
    initial_chain: ProcessorChain,
) -> Result<JoinHandle<()>> {
    let handle = thread::Builder::new()
        .name("resonance-pw".into())
        .spawn(move || {
            if let Err(e) = run_loop(cmd_rx, initial_chain) {
                tracing::error!("PipeWire thread error: {e:#}");
            }
        })?;
    Ok(handle)
}

fn run_loop(mut cmd_rx: rtrb::Consumer<AudioCommand>, mut chain: ProcessorChain) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None).context("create PW mainloop")?;
    let context =
        pw::context::ContextBox::new(mainloop.loop_(), None).context("create PW context")?;
    let core = context
        .connect(None)
        .context("connect to PipeWire daemon")?;

    // Ring buffer: capture stream → output stream (audio samples, interleaved f32)
    let (mut audio_tx, mut audio_rx) = RingBuffer::<f32>::new(RING_FRAMES * CHANNELS);

    // ── Capture stream (virtual sink — reads audio FROM the running apps) ────

    let capture = pw::stream::StreamBox::new(
        &core,
        "resonance-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "DSP",
            *pw::keys::NODE_NAME => "resonance-capture",
            *pw::keys::NODE_DESCRIPTION => "Resonance EQ capture",
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
            // Push into ring buffer; if full, drop (prefer no-alloc over blocking)
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
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut cap_param_pod,
        )
        .context("connect capture stream")?;

    // ── Playback stream (writes processed audio TO the real output device) ───

    let playback = pw::stream::StreamBox::new(
        &core,
        "resonance-playback",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "DSP",
            *pw::keys::NODE_NAME => "resonance-playback",
            *pw::keys::NODE_DESCRIPTION => "Resonance EQ playback",
        },
    )
    .context("create playback stream")?;

    let play_params = build_f32_params(SAMPLE_RATE, CHANNELS as u32);
    let mut play_param_pod = [Pod::from_bytes(&play_params).unwrap()];

    let _play_listener = playback
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            // Drain any pending parameter commands
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

                // Write f64 → f32 → output buffer (slice is &mut [u8])
                let out_f32 = cast_u8_to_f32_mut(slice);
                for (dst, &src) in out_f32.iter_mut().zip(f64_buf.iter()) {
                    *dst = src as f32;
                }
                n_frames
                // slice borrow ends here, freeing `data` for chunk_mut below
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
        "PipeWire streams connected ({}ch @ {} Hz)",
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

/// Reinterpret a `&[u8]` as `&[f32]` (F32LE, aligned).
fn cast_u8_to_f32(data: &[u8]) -> &[f32] {
    let len = data.len() / 4;
    let ptr = data.as_ptr() as *const f32;
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Reinterpret a `&mut [u8]` as `&mut [f32]` (F32LE, aligned).
fn cast_u8_to_f32_mut(data: &mut [u8]) -> &mut [f32] {
    let len = data.len() / 4;
    let ptr = data.as_mut_ptr() as *mut f32;
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}
