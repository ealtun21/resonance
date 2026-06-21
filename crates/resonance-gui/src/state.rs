//! Shared GUI state types: undo snapshots, dialogs, confirmations, the
//! narrow-layout tab selector, and per-band edit limits.

use crate::browser::Browser;
use resonance_ipc::{BandState, BandType, EffectsState};

/// A restorable snapshot of the editable chain state (undo/redo).
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) preamp_db: f64,
    pub(crate) enabled: bool,
    pub(crate) bands: Vec<BandState>,
    pub(crate) effects: EffectsState,
}

/// Editable per-band limits. Generous so extreme cuts/boosts and very narrow
/// notches are possible; the daemon/DSP impose no limit of their own.
pub(crate) const GAIN_LIMIT: f64 = 40.0;
pub(crate) const Q_LIMIT: f64 = 100.0;

pub(crate) const BAND_TYPES: [BandType; 8] = [
    BandType::Peaking,
    BandType::LowShelf,
    BandType::HighShelf,
    BandType::LowPass,
    BandType::HighPass,
    BandType::BandPass,
    BandType::Notch,
    BandType::AllPass,
];

pub(crate) enum Dialog {
    None,
    LoadPreset(Browser),
    /// Export the current chain: a directory navigator plus a filename field.
    ExportProfile(SaveDialog),
}

/// State for the Export (save-as) dialog: where to write and under what name.
pub(crate) struct SaveDialog {
    pub(crate) browser: Browser,
    /// Filename stem the user is typing (the `.toml` suffix is implicit).
    pub(crate) filename: String,
}

impl SaveDialog {
    /// Full destination path: `<cwd>/<filename>.toml`.
    pub(crate) fn target(&self) -> std::path::PathBuf {
        let name = self.filename.trim();
        let name = name.strip_suffix(".toml").unwrap_or(name);
        self.browser.cwd.join(format!("{name}.toml"))
    }
}

/// A pending destructive/overwriting profile action awaiting confirmation.
#[derive(Clone)]
pub(crate) enum Confirm {
    /// Overwrite an existing profile of this name with the current chain.
    SaveProfile(String),
    /// Delete this profile.
    DeleteProfile(String),
}
