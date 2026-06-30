//! Reference & measurement overlays for the FR graph.
//!
//! Holds the user-facing state for: a *target* curve to EQ toward (a built-in
//! like Diffuse Field / Harman, a generated `PEQdB` target, or any of those with
//! the customizer's tilt/bass/ear/treble adjustments stacked on top), an
//! optional headphone/IEM *measurement*, an optional *compare* target, and two
//! view toggles (show the raw measurement; normalise against the target). All
//! display maths live here as [`ReferenceState::series`]; `curve_view` only
//! rasterises the result.
//!
//! Curves are level-normalised for display (mean-removed) so absolute-SPL
//! measurements and relative targets both fit the graph's small ± dB axis. The
//! squig.link downloader that feeds measurements in lives in `download.rs`.

use resonance_ipc::BandState;
use resonance_ipc::curve::{self, RefCurve};
use resonance_ipc::fr::{LOG_MAX, LOG_MIN, response_db};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// A measurement saved alongside a profile (per-profile, client-side). Loading
/// the profile restores it so profiles can be A/B-compared visually. The *target*
/// is deliberately NOT bundled — only the measurement travels with a profile.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MeasurementBundle {
    pub name: String,
    pub iem: bool,
    /// Left / mono channel.
    pub left: RefCurve,
    /// Right channel, when the measurement is stereo.
    pub right: Option<RefCurve>,
}

/// A serialisable snapshot of the reference overlay — the active measurement,
/// target selection and customizer — persisted by the client across sessions so
/// a loaded measurement (and the EQ context it belongs to) survives a restart,
/// even when reference mode is currently off.
#[derive(Serialize, Deserialize, Default)]
pub struct PersistedReference {
    enabled: bool,
    show_measurement: bool,
    normalized: bool,
    #[serde(default)]
    show_bounds: bool,
    /// 0 = L+R avg, 1 = Left, 2 = Right.
    channel: u8,
    iem: bool,
    smoothing_oct: f64,
    meas_name: String,
    /// Raw per-channel measurement curves (None = no measurement loaded).
    meas_left: Option<RefCurve>,
    meas_right: Option<RefCurve>,
    /// 0 = None, 1 = File(name), 2 = `DiamondBeta`, 3 = Ultra.
    target_kind: u8,
    target_name: String,
    adj: [f64; 4],
    /// Measurements saved per profile name (restored when the profile loads).
    #[serde(default)]
    profile_meas: HashMap<String, MeasurementBundle>,
}

/// Built-in target curves embedded at build time (from `AutoEq`, MIT-licensed).
/// Users add more by dropping `.txt`/`.csv` curves into `user_curve_dir()`.
const BUILTIN_TARGETS: &[(&str, &str)] = &[
    (
        "Diffuse Field",
        include_str!("../../resonance-preset/assets/targets/Diffuse Field.txt"),
    ),
    (
        "Harman OE 2018",
        include_str!("../../resonance-preset/assets/targets/Harman OE 2018.txt"),
    ),
    (
        "Harman IE 2019",
        include_str!("../../resonance-preset/assets/targets/Harman IE 2019.txt"),
    ),
];

/// The target the Diffuse-Field-anchored generated targets build on.
const DF_NAME: &str = "Diffuse Field";

/// Listener-preference tolerance bounds at frequency `f`, returned as
/// `(below, above)` dB magnitudes around the target — an **asymmetric** band.
///
/// Shaped from the qualitative findings in headphones.com's "The Shape of IEMs
/// to Come": the midrange is tight (~±1 dB); the bass tolerates far more *boost*
/// than cut (Harman in-ear targets carry ~4 dB more bass than the headphone
/// target, and listeners reliably prefer extra low end), so the upper bound
/// flares more than the lower below ~150 Hz; the upper-mid/ear-gain and treble
/// region is contentious with high HRTF/coupler variance and no settled 5128
/// preference research, so the band widens broadly above ~2.5 kHz. These are a
/// shaped approximation (no proprietary preference dataset is reproduced), safe
/// to ship.
#[must_use]
pub fn preference_bounds(f: f64) -> (f64, f64) {
    // smoothstep between edges `e0`→`e1` (works in either direction).
    let ss = |e0: f64, e1: f64, x: f64| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let baseline = 1.0;
    let bass = ss(200.0, 40.0, f); // 0 above 200 Hz → 1 at/below 40 Hz
    let treble = ss(2500.0, 12000.0, f); // widen from ear-gain up through air
    let below = baseline + 2.4 * bass + 3.6 * treble;
    let above = baseline + 5.2 * bass + 3.6 * treble; // bass skews toward boost
    (below, above)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Avg,
    Left,
    Right,
}

impl Channel {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Channel::Avg => "L+R",
            Channel::Left => "L",
            Channel::Right => "R",
        }
    }
}

/// Which target a slot (active or compare) points at. The customizer's
/// adjustments stack on top of whichever target is active, so there's no
/// separate "custom" entry — any target is editable.
#[derive(Clone, PartialEq)]
pub enum TargetSel {
    None,
    /// A named curve from `targets` (built-in or user file).
    File(String),
    /// Diffuse Field + the `PEQdB` "Optimized Target" paper filters.
    DiamondBeta,
    Ultra,
}

/// Which tab the "Manage targets" dialog is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ManageTab {
    /// squig.link target curves (parsed from each site's config.js).
    Targets,
    /// squig.link headphone/IEM measurements, added as a target (L+R averaged).
    Measurements,
    /// The current target library — remove entries / reset to defaults.
    Yours,
}

/// One row in the "Your targets" library list: a selectable target plus how to
/// remove it (delete a user file, or hide a built-in/generated one).
pub struct LibEntry {
    pub label: String,
    /// `true` for the embedded defaults / generated `PEQdB` targets (removed by
    /// hiding, restorable via "Reset to defaults"); `false` for user files.
    pub builtin: bool,
}

/// Role of a drawn reference series — `curve_view` maps it to colour/style.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SeriesRole {
    /// The target line (dashed; flat 0 in the normalised view).
    Target,
    /// Measurement + current EQ — the line you shape onto the target.
    Result,
    /// The raw (un-EQ'd) measurement, when "show measurement" is on.
    Measurement,
}

pub struct RefSeries {
    pub role: SeriesRole,
    /// `(log10 freq, dB)` on the requested grid.
    pub pts: Vec<(f64, f64)>,
}

/// One entry in the target-curve library.
pub struct TargetItem {
    pub name: String,
    pub curve: RefCurve,
    /// Embedded default (can't be deleted); user-dir curves can.
    pub builtin: bool,
    /// File path for user curves (for deletion); `None` for built-ins.
    pub path: Option<std::path::PathBuf>,
}

pub struct ReferenceState {
    /// Master switch — off means the graph is pure EQ (no overlays, clean UI).
    pub enabled: bool,
    /// Toggle: draw the raw (un-EQ'd) measurement next to the result.
    pub show_measurement: bool,
    /// Toggle: normalised view — re-baseline so the target is a flat 0 dB line
    /// and the result shows as deviation. Only meaningful with a measurement.
    pub normalized: bool,
    /// Toggle: shade a listener-preference tolerance band around the target
    /// (tight in the mids, widening through bass/treble). A visual "stay inside
    /// this zone" aid for manual EQ.
    pub show_bounds: bool,

    /// Available target curves (built-in defaults + user `curve` dir).
    pub targets: Vec<TargetItem>,
    /// Labels hidden from the selector — built-ins/generated the user "removed"
    /// (user *files* are deleted instead). Cleared by "Reset to defaults".
    pub hidden: HashSet<String>,
    pub target_sel: TargetSel,
    /// Edit buffer for naming a saved custom target.
    pub target_name: String,
    /// Resolved active target (base + customizer; cached).
    pub target: Option<RefCurve>,

    /// Loaded measurement channels: `(left/mono, optional right)`.
    pub measurement_lr: Option<(RefCurve, Option<RefCurve>)>,
    pub measurement_name: String,
    /// True if the measurement is from an in-ear rig (picks `AutoEQ` smoothing).
    pub measurement_iem: bool,
    pub channel: Channel,
    /// 1/N-octave smoothing for the displayed measurement (0 = none).
    pub smoothing_oct: f64,
    /// Resolved measurement (channel-picked + smoothed; cached).
    pub measurement: Option<RefCurve>,

    /// The squig.link "Browse measurements" dialog is open.
    pub show_browser: bool,
    /// Search text in the browse dialog.
    pub browse_query: String,

    /// The "Manage targets" dialog is open (add from squig / remove / reset).
    pub show_manage: bool,
    pub manage_tab: ManageTab,
    /// Separate search buffers for the target-curve and measurement tabs.
    pub manage_tquery: String,
    pub manage_mquery: String,

    // ── Target customizer (stacks on the active target) ──
    pub adj_tilt: f64,
    pub adj_bass: f64,
    pub adj_ear: f64,
    pub adj_treble: f64,

    /// Measurement saved per profile name. Populated on profile save, applied on
    /// profile load (so loading a profile restores the measurement it was saved
    /// with — for visual A/B comparison). The target is not stored here. Prefer
    /// the `*_profile_meas` / `store`/`restore_measurement_for` methods over
    /// touching this directly.
    pub profile_meas: HashMap<String, MeasurementBundle>,
}

impl Default for ReferenceState {
    fn default() -> Self {
        Self {
            enabled: false,
            show_measurement: true,
            normalized: false,
            show_bounds: false,
            targets: load_targets(),
            hidden: load_hidden(),
            target_sel: TargetSel::None,
            target_name: String::new(),
            target: None,
            measurement_lr: None,
            measurement_name: String::new(),
            measurement_iem: false,
            channel: Channel::Avg,
            smoothing_oct: 1.0 / 24.0,
            measurement: None,
            show_browser: false,
            browse_query: String::new(),
            show_manage: false,
            manage_tab: ManageTab::Targets,
            manage_tquery: String::new(),
            manage_mquery: String::new(),
            adj_tilt: 0.0,
            adj_bass: 0.0,
            adj_ear: 0.0,
            adj_treble: 0.0,
            profile_meas: HashMap::new(),
        }
    }
}

impl ReferenceState {
    /// True when overlays should be drawn (enabled + something to show).
    #[must_use]
    pub fn active(&self) -> bool {
        // Overlays only make sense against a measurement — comparing the bare EQ
        // curve to a target tells you nothing.
        self.enabled && self.measurement.is_some()
    }

    /// The normalised view only applies with a measurement to deviate.
    #[must_use]
    pub fn norm_view(&self) -> bool {
        self.normalized && self.measurement.is_some()
    }

    /// Any non-zero customizer adjustment?
    fn has_adjust(&self) -> bool {
        self.adj_tilt != 0.0
            || self.adj_bass != 0.0
            || self.adj_ear != 0.0
            || self.adj_treble != 0.0
    }

    fn lookup(&self, name: &str) -> Option<&RefCurve> {
        self.targets
            .iter()
            .find(|t| t.name == name)
            .map(|t| &t.curve)
    }

    /// Selectable targets for the dropdown — only the *visible* library, i.e.
    /// everything except entries the user has removed (`hidden`). Defaults
    /// (built-ins + generated) first, then user/added curves.
    #[must_use]
    pub fn target_options(&self) -> Vec<(String, TargetSel)> {
        let visible = |name: &str| !self.hidden.contains(name);
        let mut opts = vec![("None".to_string(), TargetSel::None)];
        for t in self
            .targets
            .iter()
            .filter(|t| t.builtin && visible(&t.name))
        {
            opts.push((t.name.clone(), TargetSel::File(t.name.clone())));
        }
        if visible("PEQdB Diamond β") {
            opts.push(("PEQdB Diamond β".to_string(), TargetSel::DiamondBeta));
        }
        if visible("PEQdB Ultra") {
            opts.push(("PEQdB Ultra".to_string(), TargetSel::Ultra));
        }
        for t in self
            .targets
            .iter()
            .filter(|t| !t.builtin && visible(&t.name))
        {
            opts.push((t.name.clone(), TargetSel::File(t.name.clone())));
        }
        opts
    }

    /// The visible target library as removable rows ("Your targets" tab). Built
    /// from [`target_options`] minus the `None` entry, tagging each as a
    /// built-in/generated default (hidden to remove) or a user file (deleted).
    #[must_use]
    pub fn library_entries(&self) -> Vec<LibEntry> {
        self.target_options()
            .into_iter()
            .filter(|(_, sel)| !matches!(sel, TargetSel::None))
            .map(|(label, sel)| {
                let is_user_file = matches!(&sel, TargetSel::File(n)
                    if self.targets.iter().any(|t| &t.name == n && !t.builtin));
                LibEntry {
                    label,
                    builtin: !is_user_file,
                }
            })
            .collect()
    }

    /// Count of currently-hidden defaults (shown in the manage dialog so the
    /// "Reset to defaults" button reads as actionable).
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// Whether the active target can be removed (anything but "None").
    #[must_use]
    pub fn active_target_removable(&self) -> bool {
        !matches!(self.target_sel, TargetSel::None)
    }

    #[must_use]
    pub fn label_for(sel: &TargetSel) -> String {
        match sel {
            TargetSel::None => "None".to_string(),
            TargetSel::File(n) => n.clone(),
            TargetSel::DiamondBeta => "PEQdB Diamond β".to_string(),
            TargetSel::Ultra => "PEQdB Ultra".to_string(),
        }
    }

    #[must_use]
    pub fn target_label(&self) -> String {
        Self::label_for(&self.target_sel)
    }

    /// The selected target *before* customizer adjustments — for drawing the
    /// base curve behind the adjusted one in the customizer thumbnail.
    #[must_use]
    pub fn base_curve(&self) -> Option<RefCurve> {
        self.resolve(&self.target_sel.clone())
    }

    /// Resolve a selection into a base curve (no customizer adjustments).
    fn resolve(&self, sel: &TargetSel) -> Option<RefCurve> {
        match sel {
            TargetSel::None => None,
            TargetSel::File(name) => self.lookup(name).cloned(),
            TargetSel::DiamondBeta => Some(curve::generate_target(
                self.lookup(DF_NAME),
                0.0,
                &curve::diamond_beta_filters(),
            )),
            TargetSel::Ultra => Some(curve::generate_target(
                self.lookup(DF_NAME),
                0.0,
                &curve::ultra_filters(),
            )),
        }
    }

    pub fn set_target(&mut self, sel: TargetSel) {
        self.target_sel = sel;
        self.rebuild_target();
    }

    /// Re-resolve `self.target` = base (from selection) + customizer adjustments.
    pub fn rebuild_target(&mut self) {
        let base = self.resolve(&self.target_sel.clone());
        self.target = match base {
            None => None,
            Some(b) if self.has_adjust() => Some(curve::generate_target(
                Some(&b),
                self.adj_tilt,
                &curve::customizer_filters(self.adj_bass, self.adj_ear, self.adj_treble),
            )),
            Some(b) => Some(b),
        };
    }

    /// Re-scan the curve library (after a save / dropped-in file) and re-resolve.
    pub fn reload_targets(&mut self) {
        self.targets = load_targets();
        self.rebuild_target();
    }

    fn reset_adjust(&mut self) {
        self.adj_tilt = 0.0;
        self.adj_bass = 0.0;
        self.adj_ear = 0.0;
        self.adj_treble = 0.0;
    }

    /// Write a curve to the user library as `<name>.txt` and reload the library.
    /// Un-hides the name (adding implies showing it). Returns the sanitized name
    /// on success. Does **not** change the active selection.
    pub fn write_target(&mut self, name: &str, curve: &RefCurve) -> Option<String> {
        const GENERATED: [&str; 2] = ["PEQdB Diamond β", "PEQdB Ultra"];
        let dir = resonance_ipc::paths::user_curve_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        // Disambiguate a name that collides with a built-in or generated target:
        // `load_targets` lets the built-in win over a same-named user file, so
        // writing "Diffuse Field" verbatim would be silently shadowed (the added
        // curve never appears). Suffix "(added)" so it shows as its own entry.
        let mut name = sanitize_name(name);
        let collides = GENERATED.contains(&name.as_str())
            || self.targets.iter().any(|t| t.builtin && t.name == name);
        if collides {
            name = format!("{name} (added)");
        }
        let mut body = String::from("frequency,raw\n");
        for &(f, db) in &curve.points {
            writeln!(body, "{f:.2},{db:.3}").unwrap();
        }
        if std::fs::write(dir.join(format!("{name}.txt")), body).is_err() {
            return None;
        }
        if self.hidden.remove(&name) {
            self.save_hidden();
        }
        self.reload_targets();
        Some(name)
    }

    /// Write a curve to the library, clear the customizer, and select it (the
    /// customizer's Save flow — its adjustments are baked into the saved curve).
    pub fn save_target(&mut self, name: &str, curve: &RefCurve) {
        if let Some(name) = self.write_target(name, curve) {
            self.reset_adjust();
            self.set_target(TargetSel::File(name));
        }
    }

    /// Remove a target from the library by its selector label: delete the file
    /// for a user curve, or hide a built-in/generated default (restorable via
    /// [`reset_targets_to_defaults`](Self::reset_targets_to_defaults)).
    pub fn remove_target_label(&mut self, label: &str) {
        let user_path = self
            .targets
            .iter()
            .find(|t| t.name == label && !t.builtin)
            .and_then(|t| t.path.clone());
        if let Some(path) = user_path {
            let _ = std::fs::remove_file(path);
        } else {
            self.hidden.insert(label.to_string());
            self.save_hidden();
        }
        self.reload_targets();
        if self.target_label() == label {
            self.set_target(TargetSel::None);
        }
    }

    /// Restore the default library: un-hide every built-in/generated target.
    /// User-added curves are kept (remove those individually).
    pub fn reset_targets_to_defaults(&mut self) {
        self.hidden.clear();
        self.save_hidden();
        self.reload_targets();
    }

    fn save_hidden(&self) {
        let dir = resonance_ipc::paths::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let body = self.hidden.iter().fold(String::new(), |mut acc, n| {
            let _ = writeln!(acc, "{n}");
            acc
        });
        let _ = std::fs::write(dir.join("hidden_targets.txt"), body);
    }

    /// Snapshot the overlay for cross-session persistence.
    #[must_use]
    pub fn to_persisted(&self) -> PersistedReference {
        let (left, right) = match &self.measurement_lr {
            Some((l, r)) => (Some(l.clone()), r.clone()),
            None => (None, None),
        };
        let (target_kind, target_name) = match &self.target_sel {
            TargetSel::None => (0, String::new()),
            TargetSel::File(n) => (1, n.clone()),
            TargetSel::DiamondBeta => (2, String::new()),
            TargetSel::Ultra => (3, String::new()),
        };
        PersistedReference {
            enabled: self.enabled,
            show_measurement: self.show_measurement,
            normalized: self.normalized,
            show_bounds: self.show_bounds,
            channel: match self.channel {
                Channel::Avg => 0,
                Channel::Left => 1,
                Channel::Right => 2,
            },
            iem: self.measurement_iem,
            smoothing_oct: self.smoothing_oct,
            meas_name: self.measurement_name.clone(),
            meas_left: left,
            meas_right: right,
            target_kind,
            target_name,
            adj: [self.adj_tilt, self.adj_bass, self.adj_ear, self.adj_treble],
            profile_meas: self.profile_meas.clone(),
        }
    }

    /// Restore a persisted overlay (on GUI startup). The measurement is loaded
    /// regardless of `enabled`, so re-enabling reference mode shows the same
    /// measurement the EQ was built against.
    pub fn restore(&mut self, p: PersistedReference) {
        self.enabled = p.enabled;
        self.show_measurement = p.show_measurement;
        self.normalized = p.normalized;
        self.show_bounds = p.show_bounds;
        self.channel = match p.channel {
            1 => Channel::Left,
            2 => Channel::Right,
            _ => Channel::Avg,
        };
        self.measurement_iem = p.iem;
        if p.smoothing_oct > 0.0 {
            self.smoothing_oct = p.smoothing_oct;
        }
        self.measurement_name = p.meas_name;
        self.measurement_lr = p.meas_left.map(|l| (l, p.meas_right));
        self.adj_tilt = p.adj[0];
        self.adj_bass = p.adj[1];
        self.adj_ear = p.adj[2];
        self.adj_treble = p.adj[3];
        self.target_sel = match p.target_kind {
            1 => TargetSel::File(p.target_name),
            2 => TargetSel::DiamondBeta,
            3 => TargetSel::Ultra,
            _ => TargetSel::None,
        };
        self.profile_meas = p.profile_meas;
        self.rebuild_target();
        self.rebuild_measurement();
    }

    /// Load a local measurement file (`freq dB` text) as the active measurement.
    pub fn load_measurement_file(&mut self, path: &std::path::Path) -> bool {
        let Some(curve) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| RefCurve::parse(&t))
        else {
            return false;
        };
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("measurement")
            .to_string();
        self.enabled = true;
        self.set_measurement(name, false, curve, None);
        true
    }

    /// Import a local target file into the user library and select it.
    pub fn import_target_file(&mut self, path: &std::path::Path) -> bool {
        let Some(curve) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| RefCurve::parse(&t))
        else {
            return false;
        };
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("target")
            .to_string();
        self.save_target(&name, &curve);
        true
    }

    /// Install a freshly loaded measurement (L/mono + optional R).
    pub fn set_measurement(&mut self, name: String, iem: bool, l: RefCurve, r: Option<RefCurve>) {
        self.measurement_name = name;
        self.measurement_iem = iem;
        self.measurement_lr = Some((l, r));
        self.rebuild_measurement();
    }

    pub fn clear_measurement(&mut self) {
        self.measurement_lr = None;
        self.measurement = None;
        self.measurement_name.clear();
        self.normalized = false; // deviation view needs a measurement
    }

    // ── Per-profile measurements ────────────────────────────────────────────
    // A profile can carry the measurement it was saved with, so loading the
    // profile restores that measurement for visual A/B comparison. Only the
    // measurement is bundled — the target is never stored per profile.

    /// The current measurement as a portable bundle (`None` if none is loaded).
    #[must_use]
    pub fn measurement_bundle(&self) -> Option<MeasurementBundle> {
        let (left, right) = self.measurement_lr.clone()?;
        Some(MeasurementBundle {
            name: self.measurement_name.clone(),
            iem: self.measurement_iem,
            left,
            right,
        })
    }

    /// Apply a measurement bundle, or clear the measurement when `None`. Enables
    /// the overlay so a restored measurement is actually visible.
    pub fn apply_measurement_bundle(&mut self, bundle: Option<MeasurementBundle>) {
        match bundle {
            Some(b) => {
                self.enabled = true;
                self.set_measurement(b.name, b.iem, b.left, b.right);
            }
            None => self.clear_measurement(),
        }
    }

    /// Capture the current measurement under `profile` (or drop any stored entry
    /// when no measurement is loaded — re-saving a profile after clearing the
    /// measurement removes it).
    pub fn store_measurement_for(&mut self, profile: &str) {
        match self.measurement_bundle() {
            Some(b) => {
                self.profile_meas.insert(profile.to_string(), b);
            }
            None => {
                self.profile_meas.remove(profile);
            }
        }
    }

    /// Apply `profile`'s saved measurement, clearing the live one if the profile
    /// has none. Returns `true` if a measurement was applied (vs. cleared).
    pub fn restore_measurement_for(&mut self, profile: &str) -> bool {
        let bundle = self.profile_meas.get(profile).cloned();
        let applied = bundle.is_some();
        self.apply_measurement_bundle(bundle);
        applied
    }

    /// Whether `profile` has a stored measurement.
    #[must_use]
    pub fn has_profile_measurement(&self, profile: &str) -> bool {
        self.profile_meas.contains_key(profile)
    }

    /// Move a stored measurement when a profile is renamed.
    pub fn rename_profile_meas(&mut self, from: &str, to: &str) {
        if let Some(b) = self.profile_meas.remove(from) {
            self.profile_meas.insert(to.to_string(), b);
        }
    }

    /// Drop a stored measurement when a profile is deleted.
    pub fn remove_profile_meas(&mut self, profile: &str) {
        self.profile_meas.remove(profile);
    }

    /// Copy a stored measurement when a profile is duplicated.
    pub fn duplicate_profile_meas(&mut self, from: &str, to: &str) {
        if let Some(b) = self.profile_meas.get(from).cloned() {
            self.profile_meas.insert(to.to_string(), b);
        }
    }

    /// Re-resolve `self.measurement` from channel choice + smoothing.
    pub fn rebuild_measurement(&mut self) {
        self.measurement = self.measurement_lr.as_ref().map(|(l, r)| {
            let picked = match (self.channel, r) {
                (Channel::Left, _) | (_, None) => l.clone(),
                (Channel::Right, Some(r)) => r.clone(),
                (Channel::Avg, Some(r)) => RefCurve::average(l, r),
            };
            picked.smoothed(self.smoothing_oct)
        });
    }

    /// dB offset that centres a curve on the graph axis (mean-removed).
    fn offset(c: &RefCurve) -> f64 {
        c.norm_offset_mean(LOG_MIN, LOG_MAX)
    }

    /// Build the series to draw over `[vlo, vhi]` (log10 Hz) at `n` points,
    /// given the live EQ `bands`. Empty when nothing is active.
    pub fn series(
        &self,
        bands: &[BandState],
        sample_rate: f64,
        n: usize,
        vlo: f64,
        vhi: f64,
        na: f64,
    ) -> Vec<RefSeries> {
        if !self.active() {
            return Vec::new();
        }
        let n = n.max(2);
        // `na` (0..1) animates the normalisation: 0 = absolute curves, 1 = fully
        // re-baselined onto the target (target flattens to 0, the result and the
        // raw measurement stretch into deviation). Driving it from an eased
        // `animate_bool` lets the toggle morph instead of snapping.
        let na = na.clamp(0.0, 1.0);
        let off_t = self.target.as_ref().map(Self::offset);
        let off_m = self.measurement.as_ref().map(Self::offset);
        // Mean-remove the EQ's own broadband level too, so the result is compared
        // to the target by SHAPE only. Without this, AutoEQ's headroom/DC term
        // lives in the band gains and the result renders a few dB below the target
        // even when it's a perfect shape match (the level is just loudness, which
        // the daemon's preamp handles separately).
        let off_eq = {
            const M: usize = 96;
            let s: f64 = (0..M)
                .map(|i| {
                    let lf = LOG_MIN + (i as f64 / (M - 1) as f64) * (LOG_MAX - LOG_MIN);
                    response_db(bands, 10f64.powf(lf), sample_rate)
                })
                .sum();
            -s / M as f64
        };

        let mut target = Vec::with_capacity(n);
        let mut result = Vec::with_capacity(n);
        let mut meas = Vec::with_capacity(n);

        for i in 0..n {
            let lf = vlo + (i as f64 / (n - 1) as f64) * (vhi - vlo);
            let f = 10f64.powf(lf);
            let eq = response_db(bands, f, sample_rate);
            let t = self
                .target
                .as_ref()
                .map(|c| c.interp(f) + off_t.unwrap_or(0.0));
            let m = self
                .measurement
                .as_ref()
                .map(|c| c.interp(f) + off_m.unwrap_or(0.0));
            // Re-baseline toward the target by the animation factor: at na=0 the
            // curves are absolute, at na=1 everything is shown relative to the
            // target (which therefore flattens to 0).
            let base = na * t.unwrap_or(0.0);
            // Result = measurement shaped by the current EQ, with the EQ's mean
            // removed so only its shape contributes to the comparison.
            let res = m.unwrap_or(0.0) + eq + off_eq;

            if let Some(t) = t {
                target.push((lf, t - base)); // t·(1−na): flattens to 0 as na→1
            }
            result.push((lf, res - base));
            if self.show_measurement {
                if let Some(m) = m {
                    meas.push((lf, m - base));
                }
            }
        }

        // Faint context first, bold result last (on top).
        let mut out = Vec::new();
        if !meas.is_empty() {
            out.push(RefSeries {
                role: SeriesRole::Measurement,
                pts: meas,
            });
        }
        if !target.is_empty() {
            out.push(RefSeries {
                role: SeriesRole::Target,
                pts: target,
            });
        }
        out.push(RefSeries {
            role: SeriesRole::Result,
            pts: result,
        });
        out
    }
}

/// Filesystem-safe target name (drops path separators); falls back to a default.
fn sanitize_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    let n = n.trim();
    if n.is_empty() {
        "Custom Target".to_string()
    } else {
        n.to_string()
    }
}

/// Load the set of target labels the user has hidden from the selector.
fn load_hidden() -> HashSet<String> {
    std::fs::read_to_string(resonance_ipc::paths::config_dir().join("hidden_targets.txt"))
        .map(|b| {
            b.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Load built-in targets (embedded) plus any user curves in `user_curve_dir()`.
fn load_targets() -> Vec<TargetItem> {
    let mut out: Vec<TargetItem> = BUILTIN_TARGETS
        .iter()
        .filter_map(|(name, body)| {
            RefCurve::parse(body).map(|c| TargetItem {
                name: name.to_string(),
                curve: c,
                builtin: true,
                path: None,
            })
        })
        .collect();
    if let Ok(rd) = std::fs::read_dir(resonance_ipc::paths::user_curve_dir()) {
        for entry in rd.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("txt" | "csv")) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if out.iter().any(|t| t.name == stem) {
                continue; // built-in wins over a same-named user file
            }
            if let Some(c) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| RefCurve::parse(&t))
            {
                out.push(TargetItem {
                    name: stem.to_string(),
                    curve: c,
                    builtin: false,
                    path: Some(path),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_bounds_tight_mids_wider_extremes() {
        let (lo_m, hi_m) = preference_bounds(1000.0);
        // Midrange is tight and symmetric (~±1 dB).
        assert!((lo_m - 1.0).abs() < 1e-9 && (hi_m - 1.0).abs() < 1e-9);

        let (lo_b, hi_b) = preference_bounds(20.0);
        // Bass tolerates more boost than cut, and is wider than the mids.
        assert!(hi_b > lo_b, "bass skews toward boost");
        assert!(lo_b > lo_m && hi_b > hi_m, "bass band wider than mids");

        let (lo_t, hi_t) = preference_bounds(15000.0);
        // Treble widens symmetrically (no bass skew up there).
        assert!(lo_t > lo_m && hi_t > hi_m, "treble band wider than mids");
        assert!((lo_t - hi_t).abs() < 1e-9, "treble widening is symmetric");
    }

    #[test]
    fn default_state_inactive_and_emits_no_series() {
        let s = ReferenceState::default();
        // No measurement loaded → overlays are meaningless, so inactive.
        assert!(!s.active());
        assert!(!s.norm_view());
        assert!(
            s.series(&[], 48000.0, 64, LOG_MIN, LOG_MAX, 0.0).is_empty(),
            "an inactive reference emits no series"
        );
    }

    #[test]
    // exact-value round-trip: restored customizer floats must equal the stored ones
    #[allow(clippy::float_cmp)]
    fn persist_round_trip_preserves_toggles_and_customizer() {
        let s = ReferenceState {
            enabled: true,
            show_measurement: true,
            normalized: true,
            show_bounds: true,
            adj_tilt: -0.8,
            adj_bass: 2.0,
            ..ReferenceState::default()
        };
        let p = s.to_persisted();
        let mut s2 = ReferenceState::default();
        s2.restore(p);
        assert_eq!(s2.enabled, s.enabled);
        assert_eq!(s2.show_measurement, s.show_measurement);
        assert_eq!(s2.normalized, s.normalized);
        assert_eq!(s2.show_bounds, s.show_bounds);
        assert_eq!(s2.adj_tilt, s.adj_tilt);
        assert_eq!(s2.adj_bass, s.adj_bass);
    }

    fn curve(offset: f64) -> RefCurve {
        RefCurve {
            points: vec![
                (20.0, offset),
                (1000.0, offset + 1.0),
                (20000.0, offset - 1.0),
            ],
        }
    }

    #[test]
    fn profile_measurement_round_trip() {
        let mut s = ReferenceState::default();
        s.set_measurement("HD650".into(), false, curve(3.0), Some(curve(2.0)));
        s.store_measurement_for("Profile A");
        // Switch to a different measurement, as if another profile got loaded.
        s.set_measurement("Other".into(), true, curve(9.0), None);
        // Restoring Profile A brings its exact measurement back …
        assert!(s.restore_measurement_for("Profile A"));
        let b = s.measurement_bundle().unwrap();
        assert_eq!(b.name, "HD650");
        assert!(!b.iem);
        assert_eq!(b.left, curve(3.0));
        assert_eq!(b.right, Some(curve(2.0)));
        // … and re-enables the overlay so it's visible.
        assert!(s.enabled);
    }

    #[test]
    fn profile_measurement_does_not_bundle_target() {
        let mut s = ReferenceState::default();
        s.set_measurement("M".into(), false, curve(1.0), None);
        s.set_target(TargetSel::DiamondBeta);
        s.store_measurement_for("P");
        // A later profile uses a different target; restoring P's measurement must
        // NOT change the active target — only the measurement travels.
        s.set_target(TargetSel::Ultra);
        s.restore_measurement_for("P");
        assert!(
            matches!(s.target_sel, TargetSel::Ultra),
            "target is not part of the per-profile bundle"
        );
    }

    #[test]
    fn re_saving_profile_updates_its_measurement() {
        let mut s = ReferenceState::default();
        s.set_measurement("v1".into(), false, curve(1.0), None);
        s.store_measurement_for("P");
        // Edit the measurement while the profile is loaded, then re-save (overwrite).
        s.set_measurement("v2".into(), false, curve(5.0), None);
        s.store_measurement_for("P");
        s.clear_measurement();
        assert!(s.restore_measurement_for("P"));
        assert_eq!(s.measurement_bundle().unwrap().name, "v2");
    }

    #[test]
    fn restoring_profile_without_measurement_clears_overlay() {
        let mut s = ReferenceState::default();
        s.set_measurement("M".into(), false, curve(1.0), None);
        // "Bare" has no stored measurement → loading it clears the live one.
        assert!(!s.restore_measurement_for("Bare"));
        assert!(s.measurement_bundle().is_none());
    }

    #[test]
    fn re_saving_after_clearing_drops_the_stored_measurement() {
        let mut s = ReferenceState::default();
        s.set_measurement("M".into(), false, curve(1.0), None);
        s.store_measurement_for("P");
        assert!(s.has_profile_measurement("P"));
        s.clear_measurement();
        s.store_measurement_for("P"); // re-save with no measurement
        assert!(!s.has_profile_measurement("P"));
    }

    #[test]
    fn profile_measurement_lifecycle_rename_delete_duplicate() {
        let mut s = ReferenceState::default();
        s.set_measurement("M".into(), false, curve(2.0), None);
        s.store_measurement_for("A");
        s.duplicate_profile_meas("A", "A copy");
        assert!(s.has_profile_measurement("A copy"));
        s.rename_profile_meas("A", "B");
        assert!(!s.has_profile_measurement("A") && s.has_profile_measurement("B"));
        s.remove_profile_meas("B");
        assert!(!s.has_profile_measurement("B"));
    }

    #[test]
    fn persist_round_trip_preserves_profile_measurements() {
        let mut s = ReferenceState::default();
        s.set_measurement("HD800".into(), true, curve(4.0), Some(curve(3.0)));
        s.store_measurement_for("Studio");
        let p = s.to_persisted();
        let mut s2 = ReferenceState::default();
        s2.restore(p);
        assert!(s2.restore_measurement_for("Studio"));
        let b = s2.measurement_bundle().unwrap();
        assert_eq!(b.name, "HD800");
        assert!(b.iem);
        assert_eq!(b.right, Some(curve(3.0)));
    }
}
