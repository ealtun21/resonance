//! Scenario files (`contrib/e2e/scenarios/*.toml`): schema, expansion of
//! rate/channel lists into concrete scenarios, and tier/platform selection.

use crate::compare::CompareMode;
use anyhow::{Context, Result, bail, ensure};
use resonance_ipc::{BandState, EffectsState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Quick,
    Full,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedHop {
    pub hop: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default)]
    pub compare: CompareMode,
    /// Per-OS replacement for `compare` (`std::env::consts::OS` keys), for
    /// differences that are the platform's, e.g. a worker-built FIR kernel
    /// differing from the offline one at the 1e-13 level.
    /// Per-octave transfer-gain tolerance (dB) on OSes judged by transfer function rather than
    /// bit-equality (macOS); `None` = the runner's default.
    #[serde(default)]
    pub transfer_tolerance_db: Option<f64>,
    #[serde(default)]
    pub compare_by_os: BTreeMap<String, CompareMode>,
    #[serde(default)]
    pub resample: Vec<AllowedHop>,
    #[serde(default = "default_gap_ms")]
    pub max_gap_ms: f64,
}

impl Expect {
    /// The comparison mode that applies on `os`.
    #[must_use]
    pub fn compare_for(&self, os: &str) -> CompareMode {
        self.compare_by_os.get(os).copied().unwrap_or(self.compare)
    }
}

fn default_gap_ms() -> f64 {
    500.0
}

impl Default for Expect {
    fn default() -> Self {
        Self {
            compare: CompareMode::Exact,
            transfer_tolerance_db: None,
            compare_by_os: BTreeMap::new(),
            resample: Vec::new(),
            max_gap_ms: default_gap_ms(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    /// Pin the PipeWire graph to a new rate (devices created without a fixed
    /// rate follow it).
    ForceRate {
        rate: u32,
    },
    /// Point the daemon at `e2e_dev2` (created with this format).
    SwitchDevice {
        rate: u32,
        channels: usize,
    },
    RestartDaemon,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "EventToml")]
pub struct Event {
    pub at_secs: f64,
    pub kind: EventKind,
}

/// Flat on-disk form: `deny_unknown_fields` cannot combine with
/// `serde(flatten)`, so the kind is a string checked in `TryFrom`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventToml {
    at_secs: f64,
    kind: String,
    rate: Option<u32>,
    channels: Option<usize>,
}

impl TryFrom<EventToml> for Event {
    type Error = String;

    fn try_from(t: EventToml) -> Result<Self, String> {
        let need =
            |v: Option<u32>, f: &str| v.ok_or_else(|| format!("event `{}` needs `{f}`", t.kind));
        let kind = match t.kind.as_str() {
            "force_rate" => EventKind::ForceRate {
                rate: need(t.rate, "rate")?,
            },
            "switch_device" => EventKind::SwitchDevice {
                rate: need(t.rate, "rate")?,
                channels: t.channels.ok_or("event `switch_device` needs `channels`")?,
            },
            "restart_daemon" => EventKind::RestartDaemon,
            other => {
                return Err(format!(
                    "unknown event kind `{other}` (force_rate, switch_device, restart_daemon)"
                ));
            }
        };
        Ok(Self {
            at_secs: t.at_secs,
            kind,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileToml {
    #[serde(default)]
    preamp_db: f64,
    #[serde(default)]
    bands: Vec<BandState>,
    /// Effect name → intensity (0..=1, bipolar effects −1..=1); listed = on.
    #[serde(default)]
    effects: BTreeMap<String, f64>,
    #[serde(default)]
    dither_bits: Option<u32>,
    #[serde(default)]
    linear_phase: bool,
    /// WAV path relative to the scenario file, or `synthetic:room`.
    #[serde(default)]
    ir: Option<String>,
    /// EqualizerAPO `.txt` loaded instead of `bands` (reaches all 14 filter types).
    #[serde(default)]
    preset: Option<PathBuf>,
}

/// What the agent loads into the daemon before playing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profile {
    pub preamp_db: f64,
    pub bands: Vec<BandState>,
    pub effects: EffectsState,
    pub dither_bits: Option<u32>,
    pub linear_phase: bool,
    pub ir: Option<String>,
    pub preset: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioToml {
    id: String,
    tier: Tier,
    rates: Vec<u32>,
    channels: Vec<usize>,
    #[serde(default)]
    quick: Vec<(u32, usize)>,
    #[serde(default)]
    player_rate: Option<u32>,
    #[serde(default)]
    graph_rate: Option<u32>,
    #[serde(default = "default_body_secs")]
    body_secs: f64,
    #[serde(default)]
    platforms: Option<Vec<String>>,
    #[serde(default)]
    expected_fail: Option<String>,
    #[serde(default)]
    profile: Option<ProfileToml>,
    #[serde(default)]
    profile_file: Option<PathBuf>,
    #[serde(default)]
    expect: Expect,
    #[serde(default)]
    events: Vec<Event>,
}

fn default_body_secs() -> f64 {
    3.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileToml {
    scenario: Vec<ScenarioToml>,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    /// `<base id>@<rate>x<channels>`.
    pub id: String,
    pub rate: u32,
    pub channels: usize,
    pub player_rate: u32,
    pub graph_rate: u32,
    pub body_secs: f64,
    pub in_quick: bool,
    pub platforms: Option<Vec<String>>,
    pub expected_fail: Option<String>,
    pub profile: Profile,
    pub expect: Expect,
    pub events: Vec<Event>,
}

impl Scenario {
    /// Latency is measured only on steady, matched-rate scenarios.
    #[must_use]
    pub fn measures_latency(&self) -> bool {
        self.events.is_empty() && self.player_rate == self.rate && self.graph_rate == self.rate
    }
}

fn effects_from(map: &BTreeMap<String, f64>) -> Result<EffectsState> {
    let mut e = EffectsState::default();
    for (name, &v) in map {
        let (i, on) = match name.as_str() {
            "fidelity" => (&mut e.fidelity_intensity, &mut e.fidelity_enabled),
            "ambience" => (&mut e.ambience_intensity, &mut e.ambience_enabled),
            "surround" => (&mut e.surround_intensity, &mut e.surround_enabled),
            "dynamic_boost" => (&mut e.dynamic_boost_intensity, &mut e.dynamic_boost_enabled),
            "bass" => (&mut e.bass_intensity, &mut e.bass_enabled),
            "crossfeed" => (&mut e.crossfeed_intensity, &mut e.crossfeed_enabled),
            other => bail!(
                "unknown effect `{other}` (fidelity, ambience, surround, dynamic_boost, bass, crossfeed)"
            ),
        };
        *i = v;
        *on = true;
    }
    Ok(e)
}

fn profile_from(t: ProfileToml, base: &Path) -> Result<Profile> {
    ensure!(
        t.bands.len() <= resonance_apo::state::MAX_FILTERS,
        "{} bands; snapshots hold at most 32",
        t.bands.len()
    );
    let ir = t.ir.map(|p| {
        if p.starts_with("synthetic:") {
            p
        } else {
            base.join(p).to_string_lossy().into_owned()
        }
    });
    Ok(Profile {
        preamp_db: t.preamp_db,
        bands: t.bands,
        effects: effects_from(&t.effects)?,
        dither_bits: t.dither_bits,
        linear_phase: t.linear_phase,
        ir,
        preset: t.preset.map(|p| base.join(p)),
    })
}

fn expand(s: ScenarioToml, dir: &Path) -> Result<Vec<Scenario>> {
    ensure!(
        !s.rates.is_empty() && !s.channels.is_empty(),
        "`{}`: rates and channels must be non-empty",
        s.id
    );
    ensure!(s.body_secs > 0.0, "`{}`: body_secs must be positive", s.id);
    let profile_toml = match (s.profile, s.profile_file) {
        (Some(_), Some(_)) => bail!("`{}`: use either [profile] or profile_file, not both", s.id),
        (Some(p), None) => p,
        (None, Some(f)) => {
            let path = dir.join(&f);
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?
        }
        (None, None) => ProfileToml::default(),
    };
    let profile =
        profile_from(profile_toml, dir).with_context(|| format!("scenario `{}`", s.id))?;
    let mut out = Vec::new();
    for &rate in &s.rates {
        for &channels in &s.channels {
            ensure!(
                (1..=64).contains(&channels),
                "`{}`: channels must be 1..=64",
                s.id
            );
            out.push(Scenario {
                id: format!("{}@{rate}x{channels}", s.id),
                rate,
                channels,
                player_rate: s.player_rate.unwrap_or(rate),
                graph_rate: s.graph_rate.unwrap_or(rate),
                body_secs: s.body_secs,
                in_quick: s.tier == Tier::Quick || s.quick.contains(&(rate, channels)),
                platforms: s.platforms.clone(),
                expected_fail: s.expected_fail.clone(),
                profile: profile.clone(),
                expect: s.expect.clone(),
                events: s.events.clone(),
            });
        }
    }
    Ok(out)
}

/// Load every `*.toml` directly in `dir` (sorted by name; subdirectories
/// such as `profiles/` are not scenario files) and expand them.
pub fn load_dir(dir: &Path) -> Result<Vec<Scenario>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut all = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).with_context(|| format!("read {}", f.display()))?;
        let parsed: FileToml =
            toml::from_str(&text).with_context(|| format!("parse {}", f.display()))?;
        for s in parsed.scenario {
            all.extend(expand(s, dir).with_context(|| format!("in {}", f.display()))?);
        }
    }
    Ok(all)
}

/// `*` matches any run of characters; everything else matches literally.
#[must_use]
pub fn glob_match(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((head, rest)) => {
            s.starts_with(head)
                && (0..=s.len() - head.len()).any(|i| {
                    s.is_char_boundary(head.len() + i) && glob_match(rest, &s[head.len() + i..])
                })
        }
    }
}

#[must_use]
pub fn select<'a>(
    all: &'a [Scenario],
    tier: Tier,
    filter: Option<&str>,
    os: &str,
) -> Vec<&'a Scenario> {
    all.iter()
        .filter(|s| tier == Tier::Full || s.in_quick)
        .filter(|s| filter.is_none_or(|p| glob_match(p, &s.id)))
        .filter(|s| {
            s.platforms
                .as_ref()
                .is_none_or(|p| p.iter().any(|x| x == os))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "resonance-e2e-scn-{}-{}",
            std::process::id(),
            files[0].0.replace('/', "_")
        ));
        let _ = std::fs::remove_dir_all(&d);
        for (name, body) in files {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        d
    }

    const FLAT: &str = r#"
[[scenario]]
id = "flat"
tier = "full"
rates = [44100, 48000]
channels = [2, 8]
quick = [[48000, 2]]
"#;

    #[test]
    fn lists_expand_into_one_scenario_per_combination() {
        let s = load_dir(&dir_with(&[("a.toml", FLAT)])).unwrap();
        let ids: Vec<&str> = s.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "flat@44100x2",
                "flat@44100x8",
                "flat@48000x2",
                "flat@48000x8"
            ]
        );
        assert_eq!(s[2].player_rate, 48_000);
        assert_eq!(s[2].graph_rate, 48_000);
        assert_eq!(s[2].expect.compare, CompareMode::Exact);
    }

    #[test]
    fn quick_tier_selects_only_listed_combinations() {
        let all = load_dir(&dir_with(&[("b.toml", FLAT)])).unwrap();
        let q: Vec<&str> = select(&all, Tier::Quick, None, "linux")
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(q, ["flat@48000x2"]);
        assert_eq!(select(&all, Tier::Full, Some("*x8"), "linux").len(), 2);
    }

    #[test]
    fn platform_restriction_filters_other_oses() {
        let src = format!("{FLAT}platforms = [\"windows\"]\n");
        let all = load_dir(&dir_with(&[("c.toml", &src)])).unwrap();
        assert!(select(&all, Tier::Full, None, "linux").is_empty());
    }

    #[test]
    fn profile_file_is_resolved_and_effects_map_by_name() {
        let scn = r#"
[[scenario]]
id = "eq"
tier = "quick"
rates = [48000]
channels = [2]
profile_file = "profiles/p.toml"
"#;
        let prof = r#"
preamp_db = -3.0
effects = { bass = 0.5, crossfeed = 0.25 }
[[bands]]
band_type = "Peaking"
freq = 1000.0
gain_db = 6.0
q = 1.41
enabled = true
"#;
        let s = &load_dir(&dir_with(&[("d.toml", scn), ("profiles/p.toml", prof)])).unwrap()[0];
        assert_eq!(s.profile.bands.len(), 1);
        assert!(
            s.profile.effects.bass_enabled
                && (s.profile.effects.bass_intensity - 0.5).abs() < 1e-12
        );
        assert!(s.profile.effects.crossfeed_enabled);
        assert!(!s.profile.effects.fidelity_enabled);
    }

    #[test]
    fn events_and_tolerance_parse() {
        let scn = r#"
[[scenario]]
id = "ev"
tier = "full"
rates = [48000]
channels = [2]
[scenario.expect]
compare = { tolerance_dbfs = -120 }
max_gap_ms = 3000
resample = [{ hop = "player->graph", reason = "content rate differs" }]
[[scenario.events]]
at_secs = 1.5
kind = "force_rate"
rate = 96000
[[scenario.events]]
at_secs = 2.0
kind = "restart_daemon"
"#;
        let s = &load_dir(&dir_with(&[("e.toml", scn)])).unwrap()[0];
        assert_eq!(s.expect.compare, CompareMode::Tolerance { dbfs: -120.0 });
        assert_eq!(s.events[0].kind, EventKind::ForceRate { rate: 96_000 });
        assert_eq!(s.events[1].kind, EventKind::RestartDaemon);
        assert!(!s.measures_latency(), "event scenarios skip latency");
    }

    // Review Focus 1: typos must fail loudly.
    #[test]
    fn unknown_keys_are_rejected() {
        let src = FLAT.replace("channels =", "chanels =");
        let err = load_dir(&dir_with(&[("f.toml", &src)])).unwrap_err();
        assert!(format!("{err:#}").contains("chanels"), "{err:#}");
    }

    #[test]
    fn unknown_effect_name_is_rejected() {
        let scn = format!("{FLAT}[scenario.profile]\neffects = {{ fidelty = 0.5 }}\n");
        let err = load_dir(&dir_with(&[("g.toml", &scn)])).unwrap_err();
        assert!(format!("{err:#}").contains("fidelty"), "{err:#}");
    }

    #[test]
    fn event_with_missing_field_or_unknown_kind_is_rejected() {
        for ev in ["kind = \"force_rate\"", "kind = \"reboot\""] {
            let scn = format!("{FLAT}[[scenario.events]]\nat_secs = 1.0\n{ev}\n");
            assert!(
                load_dir(&dir_with(&[(&format!("ev-{}.toml", ev.len()), &scn)])).is_err(),
                "{ev}"
            );
        }
    }

    #[test]
    fn more_than_32_bands_is_rejected_at_load() {
        let band = "[[scenario.profile.bands]]\nband_type = \"Peaking\"\nfreq = 1000.0\ngain_db = 1.0\nq = 1.0\nenabled = true\n";
        let scn = format!("{FLAT}{}", band.repeat(33));
        let err = load_dir(&dir_with(&[("h.toml", &scn)])).unwrap_err();
        assert!(format!("{err:#}").contains("32"), "{err:#}");
    }

    #[test]
    fn glob_matches_stars() {
        assert!(glob_match("flat@*x8", "flat@96000x8"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("eq*", "flat@48000x2"));
    }

    #[test]
    fn repo_scenario_files_parse_and_quick_tier_is_not_empty() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/e2e/scenarios");
        let all = load_dir(&dir).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(!select(&all, Tier::Quick, None, "linux").is_empty());
        assert!(
            all.iter()
                .all(|s| s.profile.preset.as_ref().is_none_or(|p| p.exists()))
        );
    }
}
