//! Play into one PipeWire node while recording other nodes' monitors, all in
//! one process on one graph clock. Positions come from `pw_stream_get_time`
//! ticks, so latency is exact; a tick jump between callbacks is an
//! OS-reported discontinuity (the flake rule's evidence).

use anyhow::{Result, bail};
use pipewire as pw;
use pw::{properties::properties, spa};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub struct Play<'a> {
    pub node: &'a str,
    pub rate: u32,
    pub channels: usize,
    pub samples: &'a [f32],
}

pub struct RecordTarget<'a> {
    pub node: &'a str,
    pub rate: u32,
    pub channels: usize,
}

/// An action run on its own thread once playback passes `at_frame`.
pub struct Timed {
    pub at_frame: usize,
    pub action: Box<dyn FnOnce() + Send>,
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub node: String,
    pub channels: usize,
    pub rate: u32,
    pub first_tick: Option<u64>,
    pub samples: Vec<f32>,
    pub discontinuities: u32,
}

#[derive(Debug, Clone)]
pub struct PlayRec {
    pub play_first_tick: Option<u64>,
    pub graph_rate: u32,
    pub play_discontinuities: u32,
    pub recordings: Vec<Recording>,
}

/// Tick continuity tracker for one stream.
#[derive(Default)]
struct Clock {
    first: Option<u64>,
    last: Option<(u64, u64)>, // (ticks, expected tick advance)
    discontinuities: u32,
}

impl Clock {
    /// Record a callback at `t` that moved `frames` frames at `stream_rate`.
    /// Returns the graph rate (ticks per second) when known.
    fn tick(&mut self, t: &pw::stream::Time, frames: u64, stream_rate: u32) -> Option<u32> {
        let r = t.rate();
        let tps = (r.num > 0).then(|| u64::from(r.denom) / u64::from(r.num))?;
        let ticks = t.ticks();
        self.first.get_or_insert(ticks);
        if let Some((pt, adv)) = self.last {
            if adv > 0 && ticks != pt + adv {
                self.discontinuities += 1;
            }
        }
        // Expected advance in ticks; 0 = not exact (stream rate ≠ graph rate).
        let num = frames * tps;
        let adv = if num % u64::from(stream_rate) == 0 {
            num / u64::from(stream_rate)
        } else {
            0
        };
        self.last = Some((ticks, adv));
        u32::try_from(tps).ok()
    }
}

struct Shared {
    cursor: usize,
    play: Clock,
    recs: Vec<(Clock, Vec<f32>)>,
    graph_rate: u32,
}

fn format_param(rate: u32, channels: usize) -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(channels as u32);
    let mut pos = [0u32; spa::param::audio::MAX_CHANNELS];
    for (slot, (_, id)) in pos.iter_mut().zip(super::env::channel_positions(channels)) {
        *slot = id;
    }
    info.set_position(pos);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .expect("serialize audio format")
    .0
    .into_inner()
}

#[allow(clippy::too_many_lines)] // one linear setup → run → collect pass
pub fn play_and_record(
    play: &Play<'_>,
    record: &[RecordTarget<'_>],
    tail_frames: usize,
    mut events: Vec<Timed>,
    timeout: Duration,
) -> Result<PlayRec> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let src_frames = play.samples.len() / play.channels;
    let wants: Vec<usize> = record
        .iter()
        .map(|t| {
            ((src_frames + tail_frames) as u64 * u64::from(t.rate) / u64::from(play.rate)) as usize
        })
        .collect();
    let shared = Rc::new(RefCell::new(Shared {
        cursor: 0,
        play: Clock::default(),
        recs: record
            .iter()
            .map(|_| (Clock::default(), Vec::new()))
            .collect(),
        graph_rate: 0,
    }));
    events.sort_by_key(|e| e.at_frame);
    let events = Rc::new(RefCell::new(VecDeque::from(events)));
    let source: Rc<[f32]> = play.samples.into();

    let mut keep = Vec::new();
    // Capture streams first, so they are linked before the first played frame.
    for (j, t) in record.iter().enumerate() {
        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::TARGET_OBJECT => t.node,
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            "node.dont-reconnect" => "true",
        };
        let stream = pw::stream::StreamBox::new(&core, &format!("e2e-record-{j}"), props)?;
        let (sh, ml, want, ch, rate) = (
            Rc::clone(&shared),
            mainloop.clone(),
            wants.clone(),
            t.channels,
            t.rate,
        );
        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, ()| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let size = data.chunk().size() as usize;
                let Some(bytes) = data.data() else { return };
                let frames = size / (4 * ch);
                let mut s = sh.borrow_mut();
                if let Ok(t) = stream.time() {
                    if let Some(gr) = s.recs[j].0.tick(&t, frames as u64, rate) {
                        s.graph_rate = gr;
                    }
                }
                s.recs[j].1.extend(
                    bytes[..frames * ch * 4]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                );
                let done = s
                    .recs
                    .iter()
                    .zip(&want)
                    .all(|((_, v), &w)| v.len() / ch.max(1) >= w)
                    && s.cursor >= src_frames;
                drop(s);
                if done {
                    ml.quit();
                }
            })
            .register()?;
        let fmt = format_param(t.rate, t.channels);
        let mut params = [spa::pod::Pod::from_bytes(&fmt).expect("format pod")];
        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;
        keep.push((stream, listener));
    }

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "Production",
        *pw::keys::TARGET_OBJECT => play.node,
        "node.dont-reconnect" => "true",
    };
    let stream = pw::stream::StreamBox::new(&core, "e2e-play", props)?;
    let (sh, ev, src, ch, rate) = (
        Rc::clone(&shared),
        Rc::clone(&events),
        Rc::clone(&source),
        play.channels,
        play.rate,
    );
    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, ()| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested() as usize;
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let stride = 4 * ch;
            let Some(bytes) = data.data() else { return };
            let cap = bytes.len() / stride;
            let n = if requested > 0 {
                requested.min(cap)
            } else {
                cap
            };
            let mut s = sh.borrow_mut();
            for f in 0..n {
                for c in 0..ch {
                    let v = src.get((s.cursor + f) * ch + c).copied().unwrap_or(0.0);
                    bytes[f * stride + c * 4..f * stride + c * 4 + 4]
                        .copy_from_slice(&v.to_le_bytes());
                }
            }
            if let Ok(t) = stream.time() {
                s.play.tick(&t, n as u64, rate);
            }
            s.cursor += n;
            let cursor = s.cursor;
            drop(s);
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * n) as _;
            let mut q = ev.borrow_mut();
            while q.front().is_some_and(|e| e.at_frame <= cursor) {
                let e = q.pop_front().expect("front checked");
                std::thread::spawn(e.action);
            }
        })
        .register()?;
    let fmt = format_param(play.rate, play.channels);
    let mut params = [spa::pod::Pod::from_bytes(&fmt).expect("format pod")];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    keep.push((stream, listener));

    let started = Instant::now();
    let timed_out = Rc::new(RefCell::new(false));
    let (ml, to) = (mainloop.clone(), Rc::clone(&timed_out));
    let timer = mainloop.loop_().add_timer(move |_| {
        if started.elapsed() > timeout {
            *to.borrow_mut() = true;
            ml.quit();
        }
    });
    timer
        .update_timer(
            Some(Duration::from_millis(100)),
            Some(Duration::from_millis(100)),
        )
        .into_result()?;
    mainloop.run();
    drop(keep);

    let s = shared.borrow();
    if *timed_out.borrow() {
        let got: Vec<usize> = s
            .recs
            .iter()
            .zip(record)
            .map(|((_, v), t)| v.len() / t.channels)
            .collect();
        bail!(
            "timed out after {timeout:?}: played {} of {src_frames} frames; recorded {got:?} of {wants:?}",
            s.cursor
        );
    }
    Ok(PlayRec {
        play_first_tick: s.play.first,
        graph_rate: s.graph_rate,
        play_discontinuities: s.play.discontinuities,
        recordings: s
            .recs
            .iter()
            .zip(record)
            .zip(&wants)
            .map(|(((clock, v), t), &w)| Recording {
                node: t.node.to_owned(),
                channels: t.channels,
                rate: t.rate,
                first_tick: clock.first,
                samples: v[..(w * t.channels).min(v.len())].to_vec(),
                discontinuities: clock.discontinuities,
            })
            .collect(),
    })
}
