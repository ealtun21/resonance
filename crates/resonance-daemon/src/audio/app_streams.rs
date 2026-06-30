//! Pure, platform-agnostic helpers for the per-application stream model.
//!
//! Kept free of any backend types so the key/identity logic is unit-testable in
//! `make check` on every platform (the PipeWire/CoreAudio/WASAPI glue lives in
//! the respective backend modules).

/// Build a stable per-application key from the process binary (preferred) or a
/// fallback stream/node name, suffixed with a backend serial so two streams of
/// the same application stay distinct and the key survives re-polls.
///
/// The binary is reduced to its basename so a full path
/// (`/usr/lib/firefox/firefox`) and a bare name (`firefox`) key identically.
#[must_use]
pub fn app_key(binary: Option<&str>, fallback_name: &str, serial: u32) -> String {
    let base = binary
        .map(basename)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_name);
    format!("{base}.{serial}")
}

/// Last path segment of `path`, handling both `/` and `\` separators.
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Extract an executable basename from a WASAPI audio-session instance
/// identifier, e.g.
/// `{0.0.0.0}.{guid}|\Device\HarddiskVolume3\Program Files\app\app.exe%b{guid}`
/// → `app.exe`. Returns `None` when no path-like component is present.
///
/// Used on Windows to label sessions; kept here (pure, no Win32 types) so it is
/// unit-tested in `make check` on every platform.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[must_use]
pub fn exe_basename_from_session_id(instance_id: &str) -> Option<String> {
    // The device path follows the '|' separator; without one there is no path
    // section (e.g. a bare session guid), so there's no exe to extract. The
    // trailing "%b{guid}" after the path is metadata.
    let (_, tail) = instance_id.rsplit_once('|')?;
    let path = tail.split("%b").next().unwrap_or(tail);
    let base = basename(path);
    if base.is_empty() || !base.contains('.') {
        None
    } else {
        Some(base.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_prefers_binary_then_falls_back_to_node_name() {
        assert_eq!(app_key(Some("firefox"), "n42", 42), "firefox.42");
        assert_eq!(app_key(None, "spotify", 7), "spotify.7");
    }

    #[test]
    fn key_uses_binary_basename() {
        assert_eq!(
            app_key(Some("/usr/lib/firefox/firefox"), "n1", 1),
            "firefox.1"
        );
        assert_eq!(
            app_key(Some(r"C:\Program Files\foo\bar.exe"), "n2", 2),
            "bar.exe.2"
        );
    }

    #[test]
    fn empty_binary_falls_back_to_name() {
        assert_eq!(app_key(Some(""), "fallback", 9), "fallback.9");
    }

    #[test]
    fn exe_basename_parses_session_instance_id() {
        let id = r"{0.0.0.00000000}.{a-b}|\Device\HarddiskVolume3\Program Files\Mozilla Firefox\firefox.exe%b{c-d}";
        assert_eq!(
            exe_basename_from_session_id(id),
            Some("firefox.exe".to_owned())
        );
    }

    #[test]
    fn exe_basename_rejects_non_path_ids() {
        assert_eq!(exe_basename_from_session_id(""), None);
        assert_eq!(exe_basename_from_session_id("{0.0.0}.{guid}"), None);
    }
}
