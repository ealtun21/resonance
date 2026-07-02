pub const TABS: [&str; 6] = [
    "Profiles",
    "Mappings",
    "Devices",
    "Preferences",
    "Daemon",
    "Reference",
];

#[derive(Debug, Clone)]
pub struct SettingsState {
    pub tab: usize,
    pub cursor: usize,
    pub profiles: Vec<String>,
    pub mappings: Vec<(String, String)>,
    pub sinks: Vec<String>,
    pub text_input: Option<TextInput>,
    pub confirm: Option<ConfirmAction>,
    pub sub_picker: Option<SubPicker>,
}

#[derive(Debug, Clone)]
pub struct TextInput {
    pub buf: String,
    pub cursor: usize,
    pub purpose: TextPurpose,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TextPurpose {
    SaveProfile,
    /// Export the current chain to `<preset dir>/<name>.toml`.
    ExportProfile,
    /// Rename the profile whose current name is held inside.
    RenameProfile(String),
    PrefFps,
    PrefRefreshMs,
    PrefBandQ,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteProfile(String),
    UnmapOutput,
}

#[derive(Debug, Clone)]
pub struct SubPicker {
    pub profiles: Vec<String>,
    pub cursor: usize,
    /// None = map the currently active output; Some = route+map this specific sink.
    pub for_sink: Option<String>,
}

impl TextInput {
    pub fn new(initial: impl Into<String>, purpose: TextPurpose, label: &'static str) -> Self {
        let buf = initial.into();
        let cursor = buf.len();
        Self {
            buf,
            cursor,
            purpose,
            label,
        }
    }

    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.buf[..self.cursor]
                .char_indices()
                .last()
                .map_or(0, |(i, _)| i);
            self.buf.remove(prev);
            self.cursor = prev;
        }
    }

    pub fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.buf[..self.cursor]
                .char_indices()
                .last()
                .map_or(0, |(i, _)| i);
        }
    }

    pub fn cursor_right(&mut self) {
        if self.cursor < self.buf.len() {
            self.cursor = self.buf[self.cursor..]
                .char_indices()
                .nth(1)
                .map_or(self.buf.len(), |(i, _)| self.cursor + i);
        }
    }
}

impl SettingsState {
    pub fn new(profiles: Vec<String>, mappings: Vec<(String, String)>, sinks: Vec<String>) -> Self {
        Self {
            tab: 0,
            cursor: 0,
            profiles,
            mappings,
            sinks,
            text_input: None,
            confirm: None,
            sub_picker: None,
        }
    }

    pub fn max_cursor(&self) -> usize {
        match self.tab {
            0 => self.profiles.len().saturating_sub(1),
            1 => self.mappings.len().saturating_sub(1),
            2 => self.sinks.len().saturating_sub(1),
            // Preferences: fps / refresh / confirm / band-Q / band-type / spectrum
            // + advanced toggles (slope / scope / dither / IR / channels) + swap L/R.
            3 => 11,
            4 => 3, // Daemon: Start / Stop / Restart / Autostart
            // Reference: on / target / measurement / browse-online / autoeq /
            // show-meas / normalize / bounds / tilt / bass / ear / treble / reset.
            5 => 12,
            _ => 0,
        }
    }
}
