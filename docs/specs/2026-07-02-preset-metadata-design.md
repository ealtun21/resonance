# Preset metadata sidecar (backlog item 9)

## Problem

Presets (`.fac`, APO `.txt`) carry no author/description/tags; the TUI browser
and `resonance list` can only show file names.

## Design

New module `resonance-preset::metadata`:

```rust
pub struct PresetMeta {
    pub author: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
}
```

- Sidecar file: full preset filename + `.toml` appended
  (`Rock.fac` → `Rock.fac.toml`; `flat.txt` → `flat.txt.toml`) so `.fac` and
  `.txt` presets with the same stem never collide.
- `PresetMeta::sidecar_path(preset: &Path) -> PathBuf`
- `PresetMeta::load_for(preset: &Path) -> Option<PresetMeta>` — `None` when the
  sidecar is missing or unreadable/malformed (never fails preset loading).
- `PresetMeta::save_for(&self, preset: &Path) -> io::Result<()>`
- TOML schema: top-level optional `author`, `description`, `tags = ["…"]`.
  Unknown keys ignored (forward compatible).

Consumers (all read the sidecar client-side from the local filesystem — **no
IPC protocol change**):

- **TUI browser** (`crates/resonance-tui/src/browser.rs`): preview pane shows
  an `author / description / tags` block above the parsed preset content when
  a sidecar exists.
- **CLI `resonance list`**: append author/description to each entry line when
  present.
- **CLI `resonance meta <preset> [--author X] [--desc Y] [--tag T ...]`**: new
  client-side subcommand — with flags it writes/updates the sidecar, without
  flags it prints the current metadata. `--clear` removes the sidecar.

## Testing

- Unit: sidecar path derivation, TOML round-trip, missing sidecar → None,
  malformed TOML → None, partial fields, unknown keys ignored.
- Integration: `meta` write → `list`-style read-back through the public API;
  TUI preview rendering test if the existing browser tests support it.
- `make check` green; build + tests on Linux, Windows VM, macOS.
