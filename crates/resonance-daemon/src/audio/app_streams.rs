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
}
