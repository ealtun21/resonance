//! Devices, graph rate, PipeWire state and the daemon process, all inside
//! the e2e container. Never run this against a desktop session.

use anyhow::{Context, Result, bail, ensure};
use resonance_ipc::DaemonState;
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEVICE: &str = "e2e_dev";
pub const DEVICE2: &str = "e2e_dev2";
/// The daemon's routable null sink (`audio/pipewire.rs` `create_null_sink`).
pub const RESONANCE_SINK: &str = "resonance";
pub const PROCESSOR: &str = "resonance-processor";

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("spawn {cmd:?}"))?;
    ensure!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("timed out after {timeout:?} waiting for {what}")
}

/// SPA channel position names and ids for a width.
#[must_use]
pub fn channel_positions(channels: usize) -> Vec<(String, u32)> {
    use pipewire::spa::sys as s;
    let named: &[(&str, u32)] = match channels {
        1 => &[("MONO", s::SPA_AUDIO_CHANNEL_MONO)],
        2 => &[
            ("FL", s::SPA_AUDIO_CHANNEL_FL),
            ("FR", s::SPA_AUDIO_CHANNEL_FR),
        ],
        6 => &[
            ("FL", s::SPA_AUDIO_CHANNEL_FL),
            ("FR", s::SPA_AUDIO_CHANNEL_FR),
            ("FC", s::SPA_AUDIO_CHANNEL_FC),
            ("LFE", s::SPA_AUDIO_CHANNEL_LFE),
            ("RL", s::SPA_AUDIO_CHANNEL_RL),
            ("RR", s::SPA_AUDIO_CHANNEL_RR),
        ],
        8 => &[
            ("FL", s::SPA_AUDIO_CHANNEL_FL),
            ("FR", s::SPA_AUDIO_CHANNEL_FR),
            ("FC", s::SPA_AUDIO_CHANNEL_FC),
            ("LFE", s::SPA_AUDIO_CHANNEL_LFE),
            ("RL", s::SPA_AUDIO_CHANNEL_RL),
            ("RR", s::SPA_AUDIO_CHANNEL_RR),
            ("SL", s::SPA_AUDIO_CHANNEL_SL),
            ("SR", s::SPA_AUDIO_CHANNEL_SR),
        ],
        _ => &[],
    };
    if named.is_empty() {
        (0..channels)
            .map(|i| (format!("AUX{i}"), s::SPA_AUDIO_CHANNEL_AUX0 + i as u32))
            .collect()
    } else {
        named
            .iter()
            .map(|(n, id)| ((*n).to_string(), *id))
            .collect()
    }
}

/// A null sink acting as the "hardware" device. `rate: None` follows the graph.
pub fn create_device(name: &str, rate: Option<u32>, channels: usize) -> Result<()> {
    let pos: Vec<String> = channel_positions(channels)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    let rate_prop = rate.map(|r| format!(" audio.rate={r}")).unwrap_or_default();
    let props = format!(
        "{{ factory.name=support.null-audio-sink node.name={name} node.description={name} \
         media.class=Audio/Sink object.linger=true audio.format=F32 audio.channels={channels}{rate_prop} \
         audio.position=[{}] monitor.channel-volumes=false }}",
        pos.join(" ")
    );
    run(Command::new("pw-cli").args(["create-node", "adapter", &props]))?;
    wait_for(&format!("device {name}"), Duration::from_secs(5), || {
        Ok(node_id(&pw_dump()?, name).is_some())
    })
}

pub fn destroy_device(name: &str) -> Result<()> {
    if let Some(id) = node_id(&pw_dump()?, name) {
        run(Command::new("pw-cli").args(["destroy", &id.to_string()]))?;
    }
    wait_for(
        &format!("device {name} gone"),
        Duration::from_secs(5),
        || Ok(node_id(&pw_dump()?, name).is_none()),
    )
}

pub fn force_graph_rate(rate: Option<u32>) -> Result<()> {
    let r = rate.unwrap_or(0).to_string();
    run(Command::new("pw-metadata").args(["-n", "settings", "0", "clock.force-rate", &r]))?;
    Ok(())
}

pub fn set_default_sink(name: &str) -> Result<()> {
    let v = format!("{{ \"name\": \"{name}\" }}");
    run(Command::new("pw-metadata").args(["0", "default.configured.audio.sink", &v]))?;
    wait_for(
        &format!("default sink {name}"),
        Duration::from_secs(5),
        || Ok(default_sink(&pw_dump()?).as_deref() == Some(name)),
    )
}

pub fn pw_dump() -> Result<Value> {
    serde_json::from_str(&run(&mut Command::new("pw-dump"))?).context("parse pw-dump")
}

fn nodes(dump: &Value) -> impl Iterator<Item = &Value> {
    dump.as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["type"] == "PipeWire:Interface:Node")
}

#[must_use]
pub fn node_id(dump: &Value, name: &str) -> Option<u64> {
    nodes(dump)
        .find(|n| n["info"]["props"]["node.name"] == name)
        .and_then(|n| n["id"].as_u64())
}

#[must_use]
pub fn node_rate(dump: &Value, name: &str) -> Option<u32> {
    let n = nodes(dump).find(|n| n["info"]["props"]["node.name"] == name)?;
    n["info"]["props"]["audio.rate"]
        .as_u64()
        .or_else(|| n["info"]["params"]["Format"][0]["rate"].as_u64())
        .and_then(|r| u32::try_from(r).ok())
}

#[must_use]
pub fn default_sink(dump: &Value) -> Option<String> {
    dump.as_array()?
        .iter()
        .filter(|o| {
            o["type"] == "PipeWire:Interface:Metadata" && o["props"]["metadata.name"] == "default"
        })
        .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
        .find(|m| m["key"] == "default.audio.sink")
        .and_then(|m| m["value"]["name"].as_str().map(str::to_owned))
}

#[must_use]
pub fn daemon_running() -> bool {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "resonanced")
        })
}

pub struct Daemon {
    pid: u32,
}

impl Daemon {
    /// Start `resonanced` (stdout+stderr appended to `log`) and wait for its socket.
    pub fn start(bin: &Path, log: &Path) -> Result<Self> {
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        let child = Command::new(bin)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("start {}", bin.display()))?;
        let d = Self { pid: child.id() };
        wait_for("daemon socket", Duration::from_secs(10), || {
            Ok(resonance_ipc::transport::is_reachable())
        })?;
        Ok(d)
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// SIGTERM, then wait until no `resonanced` process remains.
    pub fn stop(self) -> Result<()> {
        run(Command::new("kill").args(["-TERM", &self.pid.to_string()]))?;
        wait_for("daemon exit", Duration::from_secs(10), || {
            Ok(!daemon_running())
        })
    }
}

/// Resonance verifiably absent: no process, no nodes, default sink = device.
pub fn verify_off(device: &str) -> Result<()> {
    ensure!(!daemon_running(), "a resonanced process is still running");
    let d = pw_dump()?;
    ensure!(
        node_id(&d, RESONANCE_SINK).is_none(),
        "\"Resonance EQ\" node still present"
    );
    ensure!(
        node_id(&d, PROCESSOR).is_none(),
        "\"{PROCESSOR}\" node still present"
    );
    let def = default_sink(&d);
    ensure!(
        def.as_deref() == Some(device),
        "default sink is {def:?}, want {device}"
    );
    Ok(())
}

/// The daemon's reported format and output must match the scenario before
/// anything is measured.
pub fn check_on_state(
    daemon_channels: usize,
    daemon_rate: f64,
    daemon_output: Option<&str>,
    device: &str,
    rate: u32,
    channels: usize,
) -> Result<()> {
    ensure!(
        daemon_channels == channels,
        "daemon runs {daemon_channels} ch, scenario wants {channels}"
    );
    ensure!(
        (daemon_rate - f64::from(rate)).abs() < 0.5,
        "daemon DSP rate {daemon_rate} Hz, scenario wants {rate}"
    );
    ensure!(
        daemon_output == Some(device),
        "daemon outputs to {daemon_output:?}, want {device}"
    );
    Ok(())
}

/// Resonance verifiably in the path: process, sink is default, state matches.
pub fn verify_on(device: &str, state: &DaemonState, rate: u32, channels: usize) -> Result<()> {
    ensure!(daemon_running(), "no resonanced process");
    let d = pw_dump()?;
    ensure!(
        node_id(&d, RESONANCE_SINK).is_some(),
        "\"Resonance EQ\" node missing"
    );
    let def = default_sink(&d);
    ensure!(
        def.as_deref() == Some(RESONANCE_SINK),
        "default sink is {def:?}, want {RESONANCE_SINK}"
    );
    check_on_state(
        state.channels,
        state.sample_rate,
        state.active_output.as_deref(),
        device,
        rate,
        channels,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dump(default: &str, nodes: &[(&str, Option<u32>)]) -> serde_json::Value {
        let mut v: Vec<serde_json::Value> = nodes.iter().enumerate().map(|(i, (name, rate))| {
            let mut props = json!({ "node.name": name, "media.class": "Audio/Sink" });
            if let Some(r) = rate { props["audio.rate"] = json!(r); }
            json!({ "id": 40 + i, "type": "PipeWire:Interface:Node", "info": { "props": props } })
        }).collect();
        v.push(json!({
            "id": 0, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
            "metadata": [{ "subject": 0, "key": "default.audio.sink", "value": { "name": default } }]
        }));
        serde_json::Value::Array(v)
    }

    #[test]
    fn parses_nodes_default_sink_and_rate() {
        let d = dump("e2e_dev", &[("e2e_dev", Some(96_000)), ("resonance", None)]);
        assert_eq!(node_id(&d, "e2e_dev"), Some(40));
        assert_eq!(node_id(&d, "missing"), None);
        assert_eq!(default_sink(&d).as_deref(), Some("e2e_dev"));
        assert_eq!(node_rate(&d, "e2e_dev"), Some(96_000));
    }

    #[test]
    fn channel_positions_cover_standard_layouts_and_aux() {
        let names = |n| {
            channel_positions(n)
                .into_iter()
                .map(|(s, _)| s)
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(names(1), "MONO");
        assert_eq!(names(2), "FL FR");
        assert_eq!(names(8), "FL FR FC LFE RL RR SL SR");
        assert_eq!(channel_positions(16).len(), 16);
        assert!(names(16).starts_with("AUX0 AUX1"));
    }

    // Review Focus 4: daemon still on the previous device format.
    #[test]
    fn on_state_rejects_a_daemon_on_the_wrong_format_or_device() {
        let dev = Some("e2e_dev");
        assert!(check_on_state(8, 96_000.0, dev, "e2e_dev", 96_000, 8).is_ok());
        let e = check_on_state(2, 96_000.0, dev, "e2e_dev", 96_000, 8).unwrap_err();
        assert!(format!("{e}").contains("2 ch"), "{e}");
        assert!(check_on_state(8, 48_000.0, dev, "e2e_dev", 96_000, 8).is_err());
        assert!(check_on_state(8, 96_000.0, Some("other"), "e2e_dev", 96_000, 8).is_err());
        assert!(check_on_state(8, 96_000.0, None, "e2e_dev", 96_000, 8).is_err());
    }
}
