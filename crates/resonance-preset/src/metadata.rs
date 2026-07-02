//! Preset metadata sidecar: optional author / description / tags stored next
//! to a preset file as `<full filename>.toml` (`Rock.fac` → `Rock.fac.toml`),
//! so `.fac` and `.txt` presets sharing a stem never collide.
//!
//! Metadata is strictly additive: a missing, unreadable or malformed sidecar
//! yields `None` and never fails preset loading. Unknown TOML keys are ignored
//! (forward compatible).

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Sidecar metadata for a preset file. All fields optional; `Default` is the
/// empty sidecar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl PresetMeta {
    /// The sidecar path for a preset: the full preset filename with `.toml`
    /// appended (not substituted), so `Rock.fac` and `Rock.txt` map to
    /// distinct sidecars.
    #[must_use]
    pub fn sidecar_path(preset: &Path) -> PathBuf {
        let mut name: OsString = preset
            .file_name()
            .map_or_else(OsString::new, ToOwned::to_owned);
        name.push(".toml");
        preset.with_file_name(name)
    }

    /// Load the sidecar for a preset. `None` when the sidecar is missing,
    /// unreadable or malformed — metadata must never fail preset loading.
    #[must_use]
    pub fn load_for(preset: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(Self::sidecar_path(preset)).ok()?;
        toml::from_str(&content).ok()
    }

    /// Write (or overwrite) the sidecar for a preset.
    ///
    /// # Errors
    ///
    /// Returns any filesystem error from writing the sidecar, or
    /// [`io::ErrorKind::InvalidData`] if the metadata cannot be serialized.
    pub fn save_for(&self, preset: &Path) -> io::Result<()> {
        let toml = toml::to_string(self).map_err(io::Error::other)?;
        std::fs::write(Self::sidecar_path(preset), toml)
    }

    /// True when no field carries any information (nothing worth displaying).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.author.is_none() && self.description.is_none() && self.tags.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh scratch dir per test so parallel tests never share sidecars.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("resonance-meta-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sidecar_path_appends_toml_to_full_filename() {
        assert_eq!(
            PresetMeta::sidecar_path(Path::new("/presets/Rock.fac")),
            PathBuf::from("/presets/Rock.fac.toml")
        );
        assert_eq!(
            PresetMeta::sidecar_path(Path::new("/presets/flat.txt")),
            PathBuf::from("/presets/flat.txt.toml")
        );
        // Same stem, different extension → distinct sidecars (no collision).
        assert_ne!(
            PresetMeta::sidecar_path(Path::new("Rock.fac")),
            PresetMeta::sidecar_path(Path::new("Rock.txt"))
        );
        // No extension at all still gets a sidecar.
        assert_eq!(
            PresetMeta::sidecar_path(Path::new("bare")),
            PathBuf::from("bare.toml")
        );
    }

    #[test]
    fn toml_round_trip_preserves_all_fields() {
        let dir = scratch("round-trip");
        let preset = dir.join("Rock.fac");
        let meta = PresetMeta {
            author: Some("Jane".into()),
            description: Some("V-shaped rock curve".into()),
            tags: vec!["rock".into(), "v-shape".into()],
        };
        meta.save_for(&preset).unwrap();
        assert_eq!(PresetMeta::load_for(&preset), Some(meta));
    }

    #[test]
    fn missing_sidecar_is_none() {
        let dir = scratch("missing");
        assert_eq!(PresetMeta::load_for(&dir.join("nothing.fac")), None);
    }

    #[test]
    fn malformed_toml_is_none() {
        let dir = scratch("malformed");
        let preset = dir.join("bad.txt");
        std::fs::write(PresetMeta::sidecar_path(&preset), "author = [unclosed").unwrap();
        assert_eq!(PresetMeta::load_for(&preset), None);
    }

    #[test]
    fn partial_fields_load_with_defaults() {
        let dir = scratch("partial");
        let preset = dir.join("part.fac");
        std::fs::write(PresetMeta::sidecar_path(&preset), "author = \"Jane\"\n").unwrap();
        let meta = PresetMeta::load_for(&preset).unwrap();
        assert_eq!(meta.author.as_deref(), Some("Jane"));
        assert_eq!(meta.description, None);
        assert!(meta.tags.is_empty());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = scratch("unknown-keys");
        let preset = dir.join("future.txt");
        std::fs::write(
            PresetMeta::sidecar_path(&preset),
            "description = \"flat\"\nrating = 5\n[extra]\nnested = true\n",
        )
        .unwrap();
        let meta = PresetMeta::load_for(&preset).unwrap();
        assert_eq!(meta.description.as_deref(), Some("flat"));
    }

    #[test]
    fn empty_meta_serializes_without_noise_and_reports_empty() {
        let meta = PresetMeta::default();
        assert!(meta.is_empty());
        assert_eq!(toml::to_string(&meta).unwrap(), "");
        assert!(
            !PresetMeta {
                tags: vec!["one".into()],
                ..PresetMeta::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn save_overwrites_existing_sidecar() {
        let dir = scratch("overwrite");
        let preset = dir.join("p.fac");
        PresetMeta {
            author: Some("first".into()),
            ..PresetMeta::default()
        }
        .save_for(&preset)
        .unwrap();
        PresetMeta {
            author: Some("second".into()),
            ..PresetMeta::default()
        }
        .save_for(&preset)
        .unwrap();
        assert_eq!(
            PresetMeta::load_for(&preset).unwrap().author.as_deref(),
            Some("second")
        );
    }
}
