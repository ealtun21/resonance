//! Reference & measurement overlays for the FR graph.
//!
//! Holds the user-facing state for: a *target* curve to EQ toward (a built-in
//! like Diffuse Field / Harman, a generated PEQdB target, or any of those with
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
use std::collections::HashSet;

/// A serialisable snapshot of the reference overlay — the active measurement,
/// target selection and customizer — persisted by the GUI across sessions so a
/// loaded measurement (and the EQ context it belongs to) survives a restart,
/// even when reference mode is currently off. Stored via eframe's app storage.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct PersistedReference {
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
    /// 0 = None, 1 = File(name), 2 = DiamondBeta, 3 = Ultra.
    target_kind: u8,
    target_name: String,
    adj: [f64; 4],
}

/// Built-in target curves embedded at build time (from AutoEq, MIT-licensed).
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
pub(crate) fn preference_bounds(f: f64) -> (f64, f64) {
    // smoothstep between edges `e0`→`e1` (works in either direction).
    let ss = |e0: f64, e1: f64, x: f64| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let base = 1.0;
    let bass = ss(200.0, 40.0, f); // 0 above 200 Hz → 1 at/below 40 Hz
    let treble = ss(2500.0, 12000.0, f); // widen from ear-gain up through air
    let below = base + 2.4 * bass + 3.6 * treble;
    let above = base + 5.2 * bass + 3.6 * treble; // bass skews toward boost
    (below, above)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Avg,
    Left,
    Right,
}

impl Channel {
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) enum TargetSel {
    None,
    /// A named curve from `targets` (built-in or user file).
    File(String),
    /// Diffuse Field + the PEQdB "Optimized Target" paper filters.
    DiamondBeta,
    Ultra,
}

/// Which tab the "Manage targets" dialog is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManageTab {
    /// squig.link target curves (parsed from each site's config.js).
    Targets,
    /// squig.link headphone/IEM measurements, added as a target (L+R averaged).
    Measurements,
    /// The current target library — remove entries / reset to defaults.
    Yours,
}

/// One row in the "Your targets" library list: a selectable target plus how to
/// remove it (delete a user file, or hide a built-in/generated one).
pub(crate) struct LibEntry {
    pub label: String,
    /// `true` for the embedded defaults / generated PEQdB targets (removed by
    /// hiding, restorable via "Reset to defaults"); `false` for user files.
    pub builtin: bool,
}

/// Role of a drawn reference series — `curve_view` maps it to colour/style.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeriesRole {
    /// The target line (dashed; flat 0 in the normalised view).
    Target,
    /// Measurement + current EQ — the line you shape onto the target.
    Result,
    /// The raw (un-EQ'd) measurement, when "show measurement" is on.
    Measurement,
}

pub(crate) struct RefSeries {
    pub role: SeriesRole,
    /// `(log10 freq, dB)` on the requested grid.
    pub pts: Vec<(f64, f64)>,
}

/// One entry in the target-curve library.
pub(crate) struct TargetItem {
    pub name: String,
    pub curve: RefCurve,
    /// Embedded default (can't be deleted); user-dir curves can.
    pub builtin: bool,
    /// File path for user curves (for deletion); `None` for built-ins.
    pub path: Option<std::path::PathBuf>,
}

pub(crate) struct ReferenceState {
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
    /// True if the measurement is from an in-ear rig (picks AutoEQ smoothing).
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
        }
    }
}

impl ReferenceState {
    /// True when overlays should be drawn (enabled + something to show).
    pub(crate) fn active(&self) -> bool {
        // Overlays only make sense against a measurement — comparing the bare EQ
        // curve to a target tells you nothing.
        self.enabled && self.measurement.is_some()
    }

    /// The normalised view only applies with a measurement to deviate.
    pub(crate) fn norm_view(&self) -> bool {
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
    pub(crate) fn target_options(&self) -> Vec<(String, TargetSel)> {
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
    pub(crate) fn library_entries(&self) -> Vec<LibEntry> {
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
    pub(crate) fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// Whether the active target can be removed (anything but "None").
    pub(crate) fn active_target_removable(&self) -> bool {
        !matches!(self.target_sel, TargetSel::None)
    }

    pub(crate) fn label_for(sel: &TargetSel) -> String {
        match sel {
            TargetSel::None => "None".to_string(),
            TargetSel::File(n) => n.clone(),
            TargetSel::DiamondBeta => "PEQdB Diamond β".to_string(),
            TargetSel::Ultra => "PEQdB Ultra".to_string(),
        }
    }

    pub(crate) fn target_label(&self) -> String {
        Self::label_for(&self.target_sel)
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

    pub(crate) fn set_target(&mut self, sel: TargetSel) {
        self.target_sel = sel;
        self.rebuild_target();
    }

    /// Re-resolve `self.target` = base (from selection) + customizer adjustments.
    pub(crate) fn rebuild_target(&mut self) {
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
    pub(crate) fn reload_targets(&mut self) {
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
    pub(crate) fn write_target(&mut self, name: &str, curve: &RefCurve) -> Option<String> {
        let dir = resonance_ipc::paths::user_curve_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        // Disambiguate a name that collides with a built-in or generated target:
        // `load_targets` lets the built-in win over a same-named user file, so
        // writing "Diffuse Field" verbatim would be silently shadowed (the added
        // curve never appears). Suffix "(added)" so it shows as its own entry.
        let mut name = sanitize_name(name);
        const GENERATED: [&str; 2] = ["PEQdB Diamond β", "PEQdB Ultra"];
        let collides = GENERATED.contains(&name.as_str())
            || self.targets.iter().any(|t| t.builtin && t.name == name);
        if collides {
            name = format!("{name} (added)");
        }
        let mut body = String::from("frequency,raw\n");
        for &(f, db) in &curve.points {
            body.push_str(&format!("{f:.2},{db:.3}\n"));
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
    pub(crate) fn save_target(&mut self, name: &str, curve: &RefCurve) {
        if let Some(name) = self.write_target(name, curve) {
            self.reset_adjust();
            self.set_target(TargetSel::File(name));
        }
    }

    /// Remove a target from the library by its selector label: delete the file
    /// for a user curve, or hide a built-in/generated default (restorable via
    /// [`reset_targets_to_defaults`](Self::reset_targets_to_defaults)).
    pub(crate) fn remove_target_label(&mut self, label: &str) {
        let user_path = self
            .targets
            .iter()
            .find(|t| t.name == label && !t.builtin)
            .and_then(|t| t.path.clone());
        match user_path {
            Some(path) => {
                let _ = std::fs::remove_file(path);
            }
            None => {
                self.hidden.insert(label.to_string());
                self.save_hidden();
            }
        }
        self.reload_targets();
        if self.target_label() == label {
            self.set_target(TargetSel::None);
        }
    }

    /// Restore the default library: un-hide every built-in/generated target.
    /// User-added curves are kept (remove those individually).
    pub(crate) fn reset_targets_to_defaults(&mut self) {
        self.hidden.clear();
        self.save_hidden();
        self.reload_targets();
    }

    fn save_hidden(&self) {
        let dir = resonance_ipc::paths::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let body: String = self.hidden.iter().map(|n| format!("{n}\n")).collect();
        let _ = std::fs::write(dir.join("hidden_targets.txt"), body);
    }

    /// Snapshot the overlay for cross-session persistence.
    pub(crate) fn to_persisted(&self) -> PersistedReference {
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
        }
    }

    /// Restore a persisted overlay (on GUI startup). The measurement is loaded
    /// regardless of `enabled`, so re-enabling reference mode shows the same
    /// measurement the EQ was built against.
    pub(crate) fn restore(&mut self, p: PersistedReference) {
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
        self.rebuild_target();
        self.rebuild_measurement();
    }

    /// Load a local measurement file (`freq dB` text) as the active measurement.
    pub(crate) fn load_measurement_file(&mut self, path: &std::path::Path) -> bool {
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
    pub(crate) fn import_target_file(&mut self, path: &std::path::Path) -> bool {
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
    pub(crate) fn set_measurement(
        &mut self,
        name: String,
        iem: bool,
        l: RefCurve,
        r: Option<RefCurve>,
    ) {
        self.measurement_name = name;
        self.measurement_iem = iem;
        self.measurement_lr = Some((l, r));
        self.rebuild_measurement();
    }

    pub(crate) fn clear_measurement(&mut self) {
        self.measurement_lr = None;
        self.measurement = None;
        self.measurement_name.clear();
        self.normalized = false; // deviation view needs a measurement
    }

    /// Re-resolve `self.measurement` from channel choice + smoothing.
    pub(crate) fn rebuild_measurement(&mut self) {
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
    pub(crate) fn series(
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
            if !matches!(ext.as_deref(), Some("txt") | Some("csv")) {
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
