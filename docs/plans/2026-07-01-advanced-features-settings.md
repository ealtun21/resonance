# Advanced-features Visibility Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-feature visibility toggles (slope, scope, dither, channels) to both the TUI and GUI so the default UI is clean and advanced controls are opt-in, with the GUI's channel controls relocated into a new Settings dialog.

**Architecture:** Pure client-side UI preferences. TUI extends its `Prefs` struct + existing Settings popup; GUI adds persisted booleans + a new `Dialog::Settings` modal. Toggles gate rendering + keybindings only — they never mutate DSP state. A compact "advanced active" status hint surfaces any hidden-but-non-default feature so nothing runs invisibly. No DSP, IPC, or preset-format changes.

**Tech Stack:** Rust, ratatui (TUI), egui/eframe (GUI), `resonance_ipc` shared types, serde/toml (TUI prefs), eframe `Storage` (GUI prefs).

**Spec:** `docs/specs/2026-07-01-advanced-features-settings-design.md`

## Global Constraints

- Conventional Commits, all lowercase (e.g. `feat(tui): ...`).
- **No AI-related content anywhere** (code, comments, commit messages, docs). No `Co-Authored-By`/AI-attribution trailers.
- `make check` (fmt --check + clippy `-D warnings` + test --all) MUST pass before every commit. Clippy pedantic is enforced workspace-wide.
- Functional style preferred (iterators, closures). Match surrounding code's comment density and idiom.
- Defaults: all four toggles **off** on a fresh install; channels controls **auto-show on `>2`-channel devices**.
- Toggles are UI-only: **never** reset or mutate DSP state when a feature is hidden.
- Neutral slope value is `12` dB/oct (`slope_db_oct: u8`); neutral scope is `BandScope::Stereo`; "no routing" is `routing.is_none()`; "no per-band channel override" is `ChannelMask::is_global(channels)`.

---

# Part A — TUI (`crates/resonance-tui`)

### Task T1: Add advanced-visibility fields to `Prefs`

**Files:**
- Modify: `crates/resonance-tui/src/prefs.rs`
- Test: `crates/resonance-tui/src/prefs.rs` (new `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: four new public bool fields on `Prefs`: `show_slope`, `show_scope`, `show_dither`, `show_channels`. All default `false`. Reuses the existing `default_false` fn.

- [ ] **Step 1: Write the failing test**

Add at the end of `crates/resonance-tui/src/prefs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_toggles_default_off() {
        // An empty config (all serde defaults) must leave every advanced
        // feature hidden for a clean first-run UI.
        let p: Prefs = toml::from_str("").unwrap();
        assert!(!p.show_slope);
        assert!(!p.show_scope);
        assert!(!p.show_dither);
        assert!(!p.show_channels);
    }

    #[test]
    fn advanced_toggles_roundtrip() {
        let mut p = Prefs::default();
        p.show_slope = true;
        p.show_channels = true;
        let s = toml::to_string(&p).unwrap();
        let back: Prefs = toml::from_str(&s).unwrap();
        assert!(back.show_slope);
        assert!(!back.show_scope);
        assert!(!back.show_dither);
        assert!(back.show_channels);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p resonance-tui advanced_toggles`
Expected: FAIL — `no field 'show_slope' on type 'Prefs'` (compile error).

- [ ] **Step 3: Add the fields**

In `crates/resonance-tui/src/prefs.rs`, inside `struct Prefs`, after the `show_sinks` field, add:

```rust
    /// Advanced-feature visibility toggles — all default off so a fresh launch
    /// shows a clean UI. Each gates a control + its keybinding; power users opt
    /// in from Settings → Preferences. Channels also auto-shows on >2ch devices
    /// (see `App::show_ch`).
    #[serde(default = "default_false")]
    pub show_slope: bool,
    #[serde(default = "default_false")]
    pub show_scope: bool,
    #[serde(default = "default_false")]
    pub show_dither: bool,
    #[serde(default = "default_false")]
    pub show_channels: bool,
```

In `impl Default for Prefs`, after `show_sinks: default_false(),` add:

```rust
            show_slope: default_false(),
            show_scope: default_false(),
            show_dither: default_false(),
            show_channels: default_false(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p resonance-tui advanced_toggles`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/resonance-tui/src/prefs.rs
git commit -m "feat(tui): add advanced-feature visibility prefs"
```

---

### Task T2: Visibility helpers + advanced-active hint on `App`

**Files:**
- Modify: `crates/resonance-tui/src/app.rs` (replace `show_ch` at ~1205-1207; add two free fns + one method; add tests)

**Interfaces:**
- Consumes: `Prefs.show_slope/show_scope/show_dither/show_channels` (Task T1).
- Produces:
  - `pub(crate) fn channels_visible(show_channels: bool, channels: usize) -> bool`
  - `pub(crate) fn advanced_hint_label(dither: bool, slope: bool, scope: bool, channels: bool) -> Option<String>`
  - `App::show_ch(&self) -> bool` (now honours the pref + `>2ch` auto-show)
  - `App::advanced_active_hint(&self) -> Option<String>`

- [ ] **Step 1: Write the failing test**

If `crates/resonance-tui/src/app.rs` already has a `#[cfg(test)] mod tests { ... }`, add the two test fns below inside it. If it does not, add this module at the end of the file:

```rust
#[cfg(test)]
mod tests {
    // (the two test fns below go here)
}
```

The two test fns:

```rust
    #[test]
    fn channels_visible_rules() {
        // Mono never shows channel controls.
        assert!(!super::channels_visible(true, 1));
        // Stereo: only when opted in.
        assert!(!super::channels_visible(false, 2));
        assert!(super::channels_visible(true, 2));
        // >2ch always shows (auto-disclosure), regardless of the pref.
        assert!(super::channels_visible(false, 6));
    }

    #[test]
    fn advanced_hint_label_lists_active() {
        assert_eq!(super::advanced_hint_label(false, false, false, false), None);
        assert_eq!(
            super::advanced_hint_label(true, false, true, false).as_deref(),
            Some("adv: dither scope")
        );
        assert_eq!(
            super::advanced_hint_label(true, true, true, true).as_deref(),
            Some("adv: dither slope scope channels")
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p resonance-tui channels_visible_rules advanced_hint_label`
Expected: FAIL — `cannot find function 'channels_visible' / 'advanced_hint_label' in module 'super'`.

- [ ] **Step 3: Add the free functions**

In `crates/resonance-tui/src/app.rs`, at module level (near the top, after imports, outside any `impl`), add:

```rust
/// Whether per-channel controls (the `Ch` column, the `c`/`w` keys) should be
/// visible: always on genuinely multichannel devices (`>2`), and opt-in on
/// stereo via the `show_channels` pref. Mono (`<2`) never shows them.
pub(crate) fn channels_visible(show_channels: bool, channels: usize) -> bool {
    channels > 2 || (show_channels && channels >= 2)
}

/// Build the compact status-bar hint naming hidden-but-active advanced
/// features, or `None` when nothing hidden is doing anything.
pub(crate) fn advanced_hint_label(
    dither: bool,
    slope: bool,
    scope: bool,
    channels: bool,
) -> Option<String> {
    let parts: Vec<&str> = [
        ("dither", dither),
        ("slope", slope),
        ("scope", scope),
        ("channels", channels),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    (!parts.is_empty()).then(|| format!("adv: {}", parts.join(" ")))
}
```

- [ ] **Step 4: Replace `show_ch` and add `advanced_active_hint`**

In `crates/resonance-tui/src/app.rs`, replace the existing `show_ch` method (~lines 1205-1207):

```rust
    pub(crate) fn show_ch(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.channels > 2)
    }
```

with:

```rust
    pub(crate) fn show_ch(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| channels_visible(self.prefs.show_channels, s.channels))
    }

    /// Compact hint for the status bar: names advanced features that are hidden
    /// (their toggle off) yet hold a non-default value, so nothing runs
    /// invisibly. `None` when every hidden feature is at its default.
    pub(crate) fn advanced_active_hint(&self) -> Option<String> {
        let s = self.state.as_ref()?;
        let dither = !self.prefs.show_dither && s.dither_bits.is_some();
        let slope = !self.prefs.show_slope
            && s.bands
                .iter()
                .any(|b| b.band_type.uses_slope() && b.slope_db_oct != 12);
        let scope = !self.prefs.show_scope
            && s.bands
                .iter()
                .any(|b| b.scope != resonance_ipc::BandScope::Stereo);
        let channels = !channels_visible(self.prefs.show_channels, s.channels)
            && (s.routing.is_some() || s.bands.iter().any(|b| !b.channels.is_global(s.channels)));
        advanced_hint_label(dither, slope, scope, channels)
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p resonance-tui channels_visible_rules advanced_hint_label`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-tui/src/app.rs
git commit -m "feat(tui): channel-visibility + advanced-active-hint helpers"
```

---

### Task T3: Gate advanced keybindings

**Files:**
- Modify: `crates/resonance-tui/src/main.rs` (Normal-mode dispatch, ~lines 168-196)

**Interfaces:**
- Consumes: `App::show_ch` (T2), `Prefs.show_slope/show_scope/show_dither` (T1).

- [ ] **Step 1: Replace the five keybinding arms**

In `crates/resonance-tui/src/main.rs`, inside `handle_normal`, replace these arms:

```rust
        KeyCode::Char('S') if band_focus => app.cycle_band_slope(),
```
```rust
        KeyCode::Char('M') if band_focus => app.cycle_band_scope(),
```
```rust
        KeyCode::Char('c') if band_focus => app.begin_select_band_channels(),
```
```rust
        KeyCode::Char('w') => app.toggle_swap_lr(),
```
```rust
        KeyCode::Char('D') => app.cycle_dither(),
```

with (respectively — keep them in the same positions, only the guards change):

```rust
        // Advanced keys are gated behind their Settings → Preferences toggle so
        // a clean default UI has no hidden shortcuts; when off they no-op.
        KeyCode::Char('S') if band_focus && app.prefs.show_slope => app.cycle_band_slope(),
```
```rust
        KeyCode::Char('M') if band_focus && app.prefs.show_scope => app.cycle_band_scope(),
```
```rust
        KeyCode::Char('c') if band_focus && app.show_ch() => app.begin_select_band_channels(),
```
```rust
        KeyCode::Char('w') if app.show_ch() => app.toggle_swap_lr(),
```
```rust
        KeyCode::Char('D') if app.prefs.show_dither => app.cycle_dither(),
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p resonance-tui`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/resonance-tui/src/main.rs
git commit -m "feat(tui): gate advanced keybindings behind visibility prefs"
```

---

### Task T4: Advanced toggles in the Settings → Preferences tab

**Files:**
- Modify: `crates/resonance-tui/src/settings.rs` (`max_cursor` tab 3)
- Modify: `crates/resonance-tui/src/ui.rs` (`render_tab_prefs`, ~2263-2335)
- Modify: `crates/resonance-tui/src/app.rs` (`settings_pref_activate` ~2028-2075; add `is_swapped_lr`)

**Interfaces:**
- Consumes: `Prefs` fields (T1), `App::toggle_swap_lr` (existing).
- Produces: `App::is_swapped_lr(&self) -> bool`. Preferences tab now has 11 rows (indices 0-10): 6 existing + slope(6)/scope(7)/dither(8)/channels(9) toggles + swap-L/R action(10).

- [ ] **Step 1: Bump the Preferences cursor range**

In `crates/resonance-tui/src/settings.rs`, in `SettingsState::max_cursor`, replace:

```rust
            3 => 5, // Preferences: fps / refresh / confirm / band-Q / band-type / spectrum
```

with:

```rust
            // Preferences: fps / refresh / confirm / band-Q / band-type / spectrum
            // + advanced toggles (slope / scope / dither / channels) + swap L/R.
            3 => 10,
```

- [ ] **Step 2: Add `is_swapped_lr` to `App`**

In `crates/resonance-tui/src/app.rs`, near `toggle_swap_lr`, add:

```rust
    /// True when the current routing is exactly the front-L/R swap matrix.
    pub(crate) fn is_swapped_lr(&self) -> bool {
        self.state.as_ref().is_some_and(|s| {
            s.channels >= 2 && s.routing.as_ref() == Some(&RoutingMatrix::swap(s.channels, 0, 1))
        })
    }
```

(`RoutingMatrix` is already imported in `app.rs` — it's used by `toggle_swap_lr`.)

- [ ] **Step 3: Extend the Preferences rows**

In `crates/resonance-tui/src/ui.rs`, in `render_tab_prefs`, replace the `let items: [(&str, String, &str); 6] = [ ... ];` array (through its closing `];`) with a `Vec` of 11 rows:

```rust
    let swap_state = if app.is_swapped_lr() { "swapped" } else { "—" };
    let items: Vec<(&str, String, &str)> = vec![
        ("FPS", prefs.fps.to_string(), "(applied next launch)"),
        (
            "Refresh ms",
            prefs.refresh_ms.to_string(),
            "(state poll interval)",
        ),
        (
            "Confirm delete",
            prefs.confirm_on_delete.to_string(),
            "(guard delete/unmap with y/n)",
        ),
        (
            "Default band Q",
            format!("{:.1}", prefs.default_band_q),
            "(Q for new EQ bands)",
        ),
        (
            "Default band type",
            prefs.default_band_type.abbrev().to_string(),
            "(type for new EQ bands, Space/Enter cycles)",
        ),
        (
            "Show spectrum",
            prefs.show_spectrum.to_string(),
            "(Space/Enter toggles; off = larger graph)",
        ),
        (
            "Show slope column",
            prefs.show_slope.to_string(),
            "(advanced: per-band filter slope + [S] key)",
        ),
        (
            "Show scope column",
            prefs.show_scope.to_string(),
            "(advanced: per-band mid/side scope + [M] key)",
        ),
        (
            "Show dither",
            prefs.show_dither.to_string(),
            "(advanced: output dither indicator + [D] key)",
        ),
        (
            "Show channels",
            prefs.show_channels.to_string(),
            "(advanced: per-band Ch column + [c]/[w] keys)",
        ),
        (
            "Swap L / R",
            swap_state.to_string(),
            "(Space/Enter swaps front L/R; needs ≥2ch)",
        ),
    ];
```

The row-rendering `for` loop below it is unchanged (it already iterates `items.iter().enumerate()`).

- [ ] **Step 4: Handle the new rows on Enter/Space**

In `crates/resonance-tui/src/app.rs`, in `settings_pref_activate`, replace the trailing arm:

```rust
            5 => {
                self.prefs.show_spectrum = !self.prefs.show_spectrum;
                self.prefs.save();
            }
            _ => {}
        }
    }
```

with:

```rust
            5 => {
                self.prefs.show_spectrum = !self.prefs.show_spectrum;
                self.prefs.save();
            }
            6 => {
                self.prefs.show_slope = !self.prefs.show_slope;
                self.prefs.save();
            }
            7 => {
                self.prefs.show_scope = !self.prefs.show_scope;
                self.prefs.save();
            }
            8 => {
                self.prefs.show_dither = !self.prefs.show_dither;
                self.prefs.save();
            }
            9 => {
                self.prefs.show_channels = !self.prefs.show_channels;
                self.prefs.save();
            }
            // Swap L/R lives here too (parity with the GUI's relocated channel
            // controls) so it's reachable even when the channels column is hidden.
            10 => self.toggle_swap_lr(),
            _ => {}
        }
    }
```

- [ ] **Step 5: Verify it compiles + run the suite**

Run: `cargo test -p resonance-tui`
Expected: builds clean; all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/resonance-tui/src/settings.rs crates/resonance-tui/src/ui.rs crates/resonance-tui/src/app.rs
git commit -m "feat(tui): advanced-feature toggles in settings preferences tab"
```

---

### Task T5: Gate the advanced UI rendering (columns, status, footer, help)

**Files:**
- Modify: `crates/resonance-tui/src/ui.rs` — `render_bands` (~1235), `render_band_row` (~1357-1374), `render_status` (~278-341), `render_footer` (~155-178), `render_help` (~76-151) and its call site.

**Interfaces:**
- Consumes: `App::show_ch` (T2), `App::advanced_active_hint` (T2), `Prefs` flags (T1).

- [ ] **Step 1: Drive the Ch column off the pref-aware helper**

In `render_bands` (`crates/resonance-tui/src/ui.rs` ~1235), replace:

```rust
    // Progressive disclosure: the per-band channel column only appears on
    // >2-channel devices so stereo users get a clean table.
    let show_ch = channels > 2;
```

with:

```rust
    // Progressive disclosure: the per-band channel column appears on >2ch
    // devices, or on stereo when the user enables "Show channels" in settings.
    let show_ch = app.show_ch();
```

- [ ] **Step 2: Gate the slope/scope suffixes in the Type cell**

In `render_band_row` (~1357-1374), replace the whole slope/scope block:

```rust
    let type_name = if full_names {
        if b.band_type.uses_slope() {
            format!("{} {}dB", b.band_type.full(), b.slope_db_oct)
        } else {
            b.band_type.full().to_string()
        }
    } else if b.band_type.uses_slope() {
        format!("{} {}", b.band_type.abbrev(), b.slope_db_oct)
    } else {
        b.band_type.abbrev().to_string()
    };
    // Stereo is the implicit default; append the scope abbrev (M/S) only when a
    // band is scoped to mid or side, so most rows stay uncluttered.
    let type_name = if b.scope == resonance_ipc::BandScope::Stereo {
        type_name
    } else {
        format!("{type_name} {}", b.scope.abbrev())
    };
```

with:

```rust
    // Slope/scope suffixes only render when their advanced toggle is on.
    let show_slope = app.prefs.show_slope && b.band_type.uses_slope();
    let type_name = if full_names {
        if show_slope {
            format!("{} {}dB", b.band_type.full(), b.slope_db_oct)
        } else {
            b.band_type.full().to_string()
        }
    } else if show_slope {
        format!("{} {}", b.band_type.abbrev(), b.slope_db_oct)
    } else {
        b.band_type.abbrev().to_string()
    };
    // Append the scope abbrev (M/S) only when scoped away from Stereo AND the
    // scope toggle is on.
    let type_name = if app.prefs.show_scope && b.scope != resonance_ipc::BandScope::Stereo {
        format!("{type_name} {}", b.scope.abbrev())
    } else {
        type_name
    };
```

(`render_band_row` already receives `app: &App` — it is the first parameter, per the call site at ~1250.)

- [ ] **Step 3: Gate the dither status indicator + add the advanced-active hint**

In `render_status` (~325-341), replace the tail of the `spans` vec literal:

```rust
        Span::styled(preamp, Style::default().fg(preamp_color)),
        sep(),
        Span::styled(dither, Style::default().fg(dither_color)),
        sep(),
    ];
```

with:

```rust
        Span::styled(preamp, Style::default().fg(preamp_color)),
        sep(),
    ];
    // Dither indicator only when its toggle is on (otherwise the advanced-active
    // hint below covers a non-default hidden dither).
    if app.prefs.show_dither {
        spans.push(Span::styled(dither, Style::default().fg(dither_color)));
        spans.push(sep());
    }
    // Compact hint when a hidden advanced feature holds a non-default value.
    if let Some(hint) = app.advanced_active_hint() {
        spans.push(Span::styled(hint, Style::default().fg(Color::Yellow)));
        spans.push(sep());
    }
```

- [ ] **Step 4: Gate footer key hints**

In `render_footer` (~155-178), replace the function body up to and including the `ctx` channel-hint block:

```rust
fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let common = "[Tab] focus  [↑↓] select  [←→] adjust  [+/-] preamp  [D] dither  [Space] toggle  [l] load  [s] settings  [o] output  [A] apps  [O] outputs  [p] power  [?] help  [q] quit";
    let mut ctx = match app.focus {
        Panel::Effects => "  •  [←→] intensity".to_string(),
        Panel::Apps => "  •  [←→] volume  [Space] mute  [A] hide".to_string(),
        Panel::Sinks => "  •  [←→] volume  [Space] mute  [O] hide".to_string(),
        Panel::Bands => "  •  [a] add  [d] del  [t] type  [S] slope  [M] scope".to_string(),
        Panel::Graph => {
            "  •  drag node: [↑↓] gain  [←→] freq  [ ][ ] select  [a/d/t/S/M] band".to_string()
        }
    };
    // Channel hints only when relevant (progressive disclosure).
    if matches!(app.focus, Panel::Bands | Panel::Graph) && app.show_ch() {
        ctx.push_str("  [c] chans");
    }
    if app.state.as_ref().is_some_and(|s| s.channels >= 2) {
        ctx.push_str("  [w] swap L/R");
    }
```

with:

```rust
fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    // The dither shortcut is only advertised when its toggle is on.
    let dither_hint = if app.prefs.show_dither { "  [D] dither" } else { "" };
    let common = format!(
        "[Tab] focus  [↑↓] select  [←→] adjust  [+/-] preamp{dither_hint}  [Space] toggle  [l] load  [s] settings  [o] output  [A] apps  [O] outputs  [p] power  [?] help  [q] quit"
    );
    // Band/graph slope + scope hints are gated behind their toggles.
    let band_adv = {
        let mut extra = String::new();
        if app.prefs.show_slope {
            extra.push_str("  [S] slope");
        }
        if app.prefs.show_scope {
            extra.push_str("  [M] scope");
        }
        extra
    };
    let mut ctx = match app.focus {
        Panel::Effects => "  •  [←→] intensity".to_string(),
        Panel::Apps => "  •  [←→] volume  [Space] mute  [A] hide".to_string(),
        Panel::Sinks => "  •  [←→] volume  [Space] mute  [O] hide".to_string(),
        Panel::Bands => format!("  •  [a] add  [d] del  [t] type{band_adv}"),
        Panel::Graph => {
            format!("  •  drag node: [↑↓] gain  [←→] freq  [ ][ ] select  [a/d/t] band{band_adv}")
        }
    };
    // Channel hints only when the channel controls are visible.
    if matches!(app.focus, Panel::Bands | Panel::Graph) && app.show_ch() {
        ctx.push_str("  [c] chans");
    }
    if app.show_ch() {
        ctx.push_str("  [w] swap L/R");
    }
```

No change is needed to the `Span::styled(format!(" {common}"), ...)` line just below — `format!` accepts the now-`String` `common` unchanged. Just verify it compiles.

- [ ] **Step 5: Gate the help-popup lines**

In `render_help` (~76), change the signature to take `app`:

```rust
fn render_help(app: &App, frame: &mut Frame, area: Rect) {
```

Then apply these three concrete edits so the advanced shortcuts are listed only when their toggle is on:

1. Change `let lines = vec![` to `let mut lines = vec![` (make the vec mutable).
2. Delete these three lines from inside the "Bands panel" section of the vec (leave `key("t", "cycle band type"),` in place):

```rust
        key("S", "cycle band slope 12/24/48 dB/oct (shelf, HP/LP)"),
        key("M", "cycle band scope stereo/mid/side (≥2ch)"),
        key("c", "channel targeting (multichannel)"),
```

3. Delete these two lines from the "Global" section of the vec:

```rust
        key("D", "cycle output dither (off / 16 / 20 / 24-bit)"),
```
```rust
        key("w", "swap L/R channels (≥2ch)"),
```

4. Immediately before `let block = Block::default()` add:

```rust
    // Advanced shortcuts are listed only when their toggle is on, matching the
    // gated keybindings (Settings → Preferences enables them).
    let mut adv: Vec<Line> = Vec::new();
    if app.prefs.show_slope {
        adv.push(key("S", "cycle band slope 12/24/48 dB/oct (shelf, HP/LP)"));
    }
    if app.prefs.show_scope {
        adv.push(key("M", "cycle band scope stereo/mid/side (≥2ch)"));
    }
    if app.show_ch() {
        adv.push(key("c", "channel targeting (multichannel)"));
        adv.push(key("w", "swap L/R channels (≥2ch)"));
    }
    if app.prefs.show_dither {
        adv.push(key("D", "cycle output dither (off / 16 / 20 / 24-bit)"));
    }
    if adv.is_empty() {
        adv.push(Line::from(Span::styled(
            "  (enable slope / scope / dither / channels in Settings → Preferences)",
            Style::default().fg(Color::DarkGray).italic(),
        )));
    }
    // Insert the advanced block just before the closing "press any key" line.
    let tail = lines.split_off(lines.len() - 2);
    lines.push(head("Advanced (opt-in)"));
    lines.extend(adv);
    lines.extend(tail);
```

(The `split_off(len - 2)` peels the trailing blank line + "press any key to close" line so the Advanced section slots in above them. If the exact trailing count differs after your edits, adjust the `- 2` so the closing italic line stays last.)

- [ ] **Step 6: Update the `render_help` call site**

Find the call `render_help(frame, ` (~line 69) and change it to pass `app` first:

```rust
        render_help(app, frame, popup_area);
```

(Use the same area argument name already present at the call site.)

- [ ] **Step 7: Verify + full suite**

Run: `cargo test -p resonance-tui && cargo build -p resonance-tui`
Expected: builds clean, tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/resonance-tui/src/ui.rs
git commit -m "feat(tui): gate advanced columns, status, footer, help behind toggles"
```

---

### Task T6: TUI `make check` + manual verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full gate**

Run: `make check`
Expected: fmt clean, clippy `-D warnings` clean, all tests pass.

- [ ] **Step 2: Manual smoke (requires a running daemon)**

Run: `resonanced` in one terminal, `cargo run -p resonance-tui` in another. Verify:
- Fresh launch (or after deleting `~/.config/resonance/tui.toml`): no slope/scope suffix in the Type column, no dither indicator in the status bar, `S`/`M`/`D` do nothing, footer omits `[D] dither` / `[S] slope` / `[M] scope`.
- Open Settings (`s`) → Preferences: toggle each of the four new rows with Space; confirm the corresponding UI element appears/disappears live.
- Set dither to 24-bit (with dither shown), then hide it: the status bar shows `adv: dither`.

- [ ] **Step 3: Commit (if any fmt fixes were applied)**

```bash
git add -A && git commit -m "style(tui): fmt" || echo "nothing to commit"
```

---

# Part B — GUI (`crates/resonance-gui`)

### Task G1: Persist advanced-visibility booleans

**Files:**
- Modify: `crates/resonance-gui/src/app.rs` — `GuiApp` struct (~335), `new` loader (~668), `save` (~1192-1200)

**Interfaces:**
- Produces: `GuiApp.show_slope`, `GuiApp.show_scope`, `GuiApp.show_dither` — persisted bools, default `false`.

- [ ] **Step 1: Add the fields**

In `crates/resonance-gui/src/app.rs`, in `struct GuiApp`, immediately after the `per_channel_eq: bool,` field, add:

```rust
    /// Advanced-feature visibility toggles (persisted; default off for a clean
    /// UI). `show_slope`/`show_scope` gate the bands-table Slope/Scope columns;
    /// `show_dither` gates the Output section. Channels controls are relocated
    /// into the Settings dialog; the per-band `Ch` column stays gated by
    /// `per_channel_eq` (auto-on for >2ch).
    pub(crate) show_slope: bool,
    pub(crate) show_scope: bool,
    pub(crate) show_dither: bool,
```

- [ ] **Step 2: Load them in the constructor**

In `crates/resonance-gui/src/app.rs`, find the `per_channel_eq: cc.storage...is_some_and(|v| v == "true"),` initializer (~668-671). Immediately after it, add:

```rust
            show_slope: cc
                .storage
                .and_then(|s| s.get_string("show_slope"))
                .is_some_and(|v| v == "true"),
            show_scope: cc
                .storage
                .and_then(|s| s.get_string("show_scope"))
                .is_some_and(|v| v == "true"),
            show_dither: cc
                .storage
                .and_then(|s| s.get_string("show_dither"))
                .is_some_and(|v| v == "true"),
```

- [ ] **Step 3: Persist them on save**

In `crates/resonance-gui/src/app.rs`, in `fn save`, after `storage.set_string("per_channel_eq", self.per_channel_eq.to_string());` add:

```rust
        storage.set_string("show_slope", self.show_slope.to_string());
        storage.set_string("show_scope", self.show_scope.to_string());
        storage.set_string("show_dither", self.show_dither.to_string());
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p resonance-gui`
Expected: builds clean.

- [ ] **Step 5: Commit**

```bash
git add crates/resonance-gui/src/app.rs
git commit -m "feat(gui): persist advanced-feature visibility toggles"
```

---

### Task G2: New `Dialog::Settings` modal + gear button + relocate theme

**Files:**
- Modify: `crates/resonance-gui/src/state.rs` — `Dialog` enum (~32-43)
- Modify: `crates/resonance-gui/src/ui/dialogs.rs` — new `settings_dialog` method
- Modify: `crates/resonance-gui/src/app.rs` — `render_dialogs` (~1390-1402)
- Modify: `crates/resonance-gui/src/ui/toolbar.rs` — `tb_settings` button, `Grp` enum + row (~57-176), remove Theme from `overflow_menu` (~438-445)

**Interfaces:**
- Consumes: `GuiApp.show_slope/show_scope/show_dither` (G1), `channels_section` (existing, `devices.rs`), `set_theme` (existing, `app.rs:822`), `Theme::ALL` (existing).
- Produces: `Dialog::Settings` variant; `GuiApp::settings_dialog(&mut self, ctx)`; `GuiApp::tb_settings(&mut self, ui)`.

- [ ] **Step 1: Add the `Settings` dialog variant**

In `crates/resonance-gui/src/state.rs`, in `enum Dialog`, add a unit variant after `None,`:

```rust
    /// App settings: advanced-feature toggles, channel controls, theme.
    Settings,
```

- [ ] **Step 2: Add the `settings_dialog` method**

In `crates/resonance-gui/src/ui/dialogs.rs`, add a new method inside the same `impl GuiApp` block that holds `help_dialog` (place it directly after `help_dialog`):

```rust
    /// App settings modal: advanced-feature visibility toggles, the relocated
    /// channel controls, and the theme picker (moved out of the overflow menu).
    pub(crate) fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, crate::state::Dialog::Settings) {
            return;
        }
        let mut open = true;
        let state = self.state.clone();
        dialog_window(ctx, "Settings")
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Advanced features").strong());
                    ui.weak("Hidden by default to keep the main view clean.");
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.show_slope, "Filter slope column (12/24/48 dB/oct)");
                    ui.checkbox(&mut self.show_scope, "Stereo scope column (Mid/Side)");
                    ui.checkbox(&mut self.show_dither, "Output dither section");

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Channels").strong());
                    if let Some(s) = &state {
                        if s.channels >= 2 {
                            self.channels_section(ui, s);
                        } else {
                            ui.weak("Stereo or multichannel output required.");
                        }
                    } else {
                        ui.weak("Connect the daemon to configure channels.");
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Theme").strong());
                    let cctx = ui.ctx().clone();
                    for t in Theme::ALL {
                        if ui.selectable_label(self.theme == t, t.label()).clicked() {
                            self.set_theme(&cctx, t);
                        }
                    }
                });
            });
        if !open {
            self.dialog = crate::state::Dialog::None;
        }
    }
```

At the top of `crates/resonance-gui/src/ui/dialogs.rs`, ensure `Theme` is in scope. If it is not already imported, add:

```rust
use crate::theme::Theme;
```

- [ ] **Step 3: Register the dialog in `render_dialogs`**

In `crates/resonance-gui/src/app.rs`, in `render_dialogs`, add after `self.help_dialog(ctx);`:

```rust
        self.settings_dialog(ctx);
```

- [ ] **Step 4: Add the gear button**

In `crates/resonance-gui/src/ui/toolbar.rs`, add a method next to `tb_help` (~363):

```rust
    fn tb_settings(&mut self, ui: &mut egui::Ui) {
        if kit::icon_btn(ui, Icon::Gear, kit::CTRL_H, "Settings") {
            self.dialog = crate::state::Dialog::Settings;
        }
    }
```

- [ ] **Step 5: Wire the gear into the toolbar row**

In `crates/resonance-gui/src/ui/toolbar.rs`, in `toolbar`:

1. Add `Settings` to the `Grp` enum:

```rust
        enum Grp {
            Power,
            Preamp,
            Output,
            History,
            Daemon,
            Settings,
            Help,
            Overflow,
        }
```

2. Account for its width. Replace:

```rust
        let w_help = 28.0; // ? help icon button
        let w_overflow = 28.0; // ☰ icon menu button
```

with:

```rust
        let w_settings = 28.0; // ⚙ settings icon button
        let w_help = 28.0; // ? help icon button
        let w_overflow = 28.0; // ☰ icon menu button
```

and replace:

```rust
        let base = w_power + w_pre_min + w_help + w_overflow + 4.0 * unit;
```

with:

```rust
        let base = w_power + w_pre_min + w_settings + w_help + w_overflow + 5.0 * unit;
```

3. Push it into the groups vec, right before `groups.push(Grp::Help);`:

```rust
            groups.push(Grp::Settings);
            groups.push(Grp::Help);
```

4. Add the match arm alongside `Grp::Help => self.tb_help(ui),`:

```rust
                    Grp::Settings => self.tb_settings(ui),
```

- [ ] **Step 6: Remove Theme from the overflow menu**

In `crates/resonance-gui/src/ui/toolbar.rs`, in `overflow_menu` (~438-445), delete the Theme block (now in the Settings dialog):

```rust
                kit::menu_caption(ui, "Theme");
                let ctx = ui.ctx().clone();
                for t in Theme::ALL {
                    if kit::menu_item(ui, t.label(), self.theme == t) {
                        self.set_theme(&ctx, t);
                    }
                }
```

- [ ] **Step 7: Verify it compiles + demo-render**

Run: `cargo build -p resonance-gui`
Expected: builds clean. If `Theme`/`Icon` import errors surface in `toolbar.rs` after removing the loop, they were only used by the deleted block — remove the now-unused `use` if clippy flags it (Step in G5's `make check` will catch this).

- [ ] **Step 8: Commit**

```bash
git add crates/resonance-gui/src/state.rs crates/resonance-gui/src/ui/dialogs.rs crates/resonance-gui/src/app.rs crates/resonance-gui/src/ui/toolbar.rs
git commit -m "feat(gui): settings dialog with advanced toggles, channels, theme"
```

---

### Task G3: Gate the Slope/Scope columns + relocate Channels / gate Output

**Files:**
- Modify: `crates/resonance-gui/src/ui/bands.rs` — `BandColumns::resolve` (~67-101), `bands_section` caller (~205); add test
- Modify: `crates/resonance-gui/src/ui/layout.rs` — wide (~206-215) + narrow (~280-296)

**Interfaces:**
- Consumes: `GuiApp.show_slope/show_scope/show_dither` (G1).
- Produces: `BandColumns::resolve(avail, gap, show_ch, show_slope_pref, show_scope_pref)`.

- [ ] **Step 1: Write the failing test**

In `crates/resonance-gui/src/ui/bands.rs`, add at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::BandColumns;

    #[test]
    fn slope_scope_columns_require_prefs() {
        // Prefs off ⇒ columns hidden even on a very wide table.
        let c = BandColumns::resolve(1000.0, 8.0, false, false, false);
        assert!(!c.show_slope);
        assert!(!c.show_scope);

        // Prefs on + wide ⇒ both shown.
        let c = BandColumns::resolve(1000.0, 8.0, false, true, true);
        assert!(c.show_slope);
        assert!(c.show_scope);

        // Prefs on but mid-narrow (>=410, <464): scope fits, slope doesn't.
        let c = BandColumns::resolve(430.0, 8.0, false, true, true);
        assert!(!c.show_slope);
        assert!(c.show_scope);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p resonance-gui slope_scope_columns_require_prefs`
Expected: FAIL — `resolve` takes 3 args, not 5 (compile error).

- [ ] **Step 3: Thread the prefs into `resolve`**

In `crates/resonance-gui/src/ui/bands.rs`, change the `resolve` signature and the two width computations. Replace:

```rust
    fn resolve(avail: f32, gap: f32, show_ch: bool) -> Self {
        // Collapse columns as the table narrows: drop the gain graph first, then
        // the Slope selector, then the Scope selector, then the Type combo.
        let show_graph = avail >= 560.0;
        let show_slope = avail >= 464.0;
        let show_scope = avail >= 410.0;
        let show_type = avail >= 360.0;
```

with:

```rust
    fn resolve(
        avail: f32,
        gap: f32,
        show_ch: bool,
        show_slope_pref: bool,
        show_scope_pref: bool,
    ) -> Self {
        // Collapse columns as the table narrows: drop the gain graph first, then
        // the Slope selector, then the Scope selector, then the Type combo. Slope
        // and Scope are additionally gated behind their advanced-visibility prefs.
        let show_graph = avail >= 560.0;
        let show_slope = show_slope_pref && avail >= 464.0;
        let show_scope = show_scope_pref && avail >= 410.0;
        let show_type = avail >= 360.0;
```

- [ ] **Step 4: Update the caller**

In `crates/resonance-gui/src/ui/bands.rs`, in `bands_section` (~205), replace:

```rust
        let cols = BandColumns::resolve(avail, kit::SP_S, show_ch);
```

with:

```rust
        let cols = BandColumns::resolve(avail, kit::SP_S, show_ch, self.show_slope, self.show_scope);
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p resonance-gui slope_scope_columns_require_prefs`
Expected: PASS.

- [ ] **Step 6: Gate Output + remove Channels in the wide layout**

In `crates/resonance-gui/src/ui/layout.rs` (~206-215), replace:

```rust
                        // Output stage (dither) sits directly under the effects rack.
                        ui.add_space(12.0);
                        section_hint(ui, "Output", "dither", |ui| {
                            self.output_section(ui, s);
                        });
                        // Channels sits under Effects (matches the design mock).
                        // Multi-channel-only — stereo users still get the L/R swap.
                        if s.channels >= 2 {
                            ui.add_space(12.0);
                            section(ui, "Channels", |ui| self.channels_section(ui, s));
                        }
```

with:

```rust
                        // Output stage (dither) — advanced, off by default. The
                        // Channels controls now live in the Settings dialog (gear
                        // icon) to keep the main view uncluttered.
                        if self.show_dither {
                            ui.add_space(12.0);
                            section_hint(ui, "Output", "dither", |ui| {
                                self.output_section(ui, s);
                            });
                        }
```

- [ ] **Step 7: Gate Output + remove Channels in the narrow layout**

In `crates/resonance-gui/src/ui/layout.rs` (~280-296), replace:

```rust
                    ui.add_space(GAP);
                    section_hint(ui, "Output", "dither", |ui| {
                        self.output_section(ui, s);
                    });
```

with:

```rust
                    if self.show_dither {
                        ui.add_space(GAP);
                        section_hint(ui, "Output", "dither", |ui| {
                            self.output_section(ui, s);
                        });
                    }
```

and replace:

```rust
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    if s.channels >= 2 {
                        ui.add_space(GAP);
                        section(ui, "Channels", |ui| self.channels_section(ui, s));
                    }
```

with:

```rust
                    section(ui, "EQ bands", |ui| self.bands_section(ui, s));
                    // Channels controls relocated to the Settings dialog.
```

- [ ] **Step 8: Verify it compiles**

Run: `cargo build -p resonance-gui`
Expected: builds clean. If `section` is now unused in `layout.rs`, clippy (G5) will flag it — remove the unused import then.

- [ ] **Step 9: Commit**

```bash
git add crates/resonance-gui/src/ui/bands.rs crates/resonance-gui/src/ui/layout.rs
git commit -m "feat(gui): gate slope/scope columns + relocate channels, gate output"
```

---

### Task G4: Advanced-active hint in the GUI status bar

**Files:**
- Modify: `crates/resonance-gui/src/app.rs` — add `advanced_hint_label` free fn + `advanced_active_hint` method; add test
- Modify: `crates/resonance-gui/src/ui/toolbar.rs` — `status_bar` (~588-660)

**Interfaces:**
- Consumes: `GuiApp.show_slope/show_scope/show_dither`, `per_channel_eq` (existing).
- Produces: `advanced_hint_label(...)`, `GuiApp::advanced_active_hint(&self) -> Option<String>`.

- [ ] **Step 1: Write the failing test**

In `crates/resonance-gui/src/app.rs`, add at the end of the file:

```rust
#[cfg(test)]
mod hint_tests {
    use super::advanced_hint_label;

    #[test]
    fn label_lists_active_features() {
        assert_eq!(advanced_hint_label(false, false, false, false), None);
        assert_eq!(
            advanced_hint_label(true, false, true, false).as_deref(),
            Some("adv: dither scope")
        );
        assert_eq!(
            advanced_hint_label(true, true, true, true).as_deref(),
            Some("adv: dither slope scope channels")
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p resonance-gui label_lists_active_features`
Expected: FAIL — `cannot find function 'advanced_hint_label'`.

- [ ] **Step 3: Add the free fn + method**

In `crates/resonance-gui/src/app.rs`, at module level (outside any `impl`), add:

```rust
/// Build the compact status-bar hint naming hidden-but-active advanced
/// features, or `None` when nothing hidden is doing anything.
pub(crate) fn advanced_hint_label(
    dither: bool,
    slope: bool,
    scope: bool,
    channels: bool,
) -> Option<String> {
    let parts: Vec<&str> = [
        ("dither", dither),
        ("slope", slope),
        ("scope", scope),
        ("channels", channels),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    (!parts.is_empty()).then(|| format!("adv: {}", parts.join(" ")))
}
```

Then, inside `impl GuiApp` (near other small helpers), add:

```rust
    /// Names advanced features that are hidden yet non-default, so nothing runs
    /// invisibly. `None` when every hidden feature is at its default.
    pub(crate) fn advanced_active_hint(&self) -> Option<String> {
        let s = self.state.as_ref()?;
        // The per-band Ch column is visible on >2ch or when per-channel EQ is on.
        let ch_visible = s.channels > 2 || (self.per_channel_eq && s.channels >= 2);
        let dither = !self.show_dither && s.dither_bits.is_some();
        let slope = !self.show_slope
            && s.bands
                .iter()
                .any(|b| b.band_type.uses_slope() && b.slope_db_oct != 12);
        let scope = !self.show_scope
            && s.bands
                .iter()
                .any(|b| b.scope != resonance_ipc::BandScope::Stereo);
        let channels =
            !ch_visible && s.bands.iter().any(|b| !b.channels.is_global(s.channels));
        advanced_hint_label(dither, slope, scope, channels)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p resonance-gui label_lists_active_features`
Expected: PASS.

- [ ] **Step 5: Render the hint in the status bar**

In `crates/resonance-gui/src/ui/toolbar.rs`, in `status_bar`, at the very end of the `ui.horizontal(|ui| { ... })` closure body (after the last meter `seg(...)` call, before the closure closes), add:

```rust
            // Compact hint when a hidden advanced feature holds a non-default
            // value (e.g. dither on while its section is hidden).
            if let Some(hint) = self.advanced_active_hint() {
                let acc = kit::tokens(ui).accent;
                seg(ui, "", &hint, acc);
            }
```

(`self.advanced_active_hint()` borrows `self` immutably; `seg` is a local closure that does not touch `self`, so there is no borrow conflict. If the accent token name differs, use the token used elsewhere for emphasis — see the `t.accent` usage in `bands.rs`.)

- [ ] **Step 6: Verify it compiles**

Run: `cargo build -p resonance-gui`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add crates/resonance-gui/src/app.rs crates/resonance-gui/src/ui/toolbar.rs
git commit -m "feat(gui): advanced-active status hint for hidden features"
```

---

### Task G5: GUI `make check` + manual verification

**Files:** none (verification only); minor unused-import cleanup if clippy flags it.

- [ ] **Step 1: Run the full gate**

Run: `make check`
Expected: fmt clean, clippy `-D warnings` clean, all tests pass. Fix any unused `use`/`fn` clippy flags introduced by the removed Theme loop (toolbar.rs) or removed `section`/`channels_section` call sites (layout.rs) — delete only genuinely-unused imports; `channels_section` is still used by the Settings dialog.

- [ ] **Step 2: Manual smoke (Xvfb screenshot harness or a live session)**

Use `contrib/dev/uishot.sh` (or run `cargo run -p resonance-gui` with a daemon). Verify:
- Fresh profile (clear egui storage): no Slope/Scope columns in the bands table, no Output section, no Channels section in the main view; the toolbar shows a gear icon.
- Click the gear → Settings dialog opens with Advanced-features checkboxes, Channels controls, and the Theme picker. The overflow (☰) menu no longer lists Theme.
- Toggle each advanced checkbox: the Slope column, Scope column, and Output section appear/disappear live.
- With Output hidden but dither set to 24-bit (set it, then hide), the status bar shows `adv: dither`.
- Channel swap + per-channel-EQ toggle work from inside the Settings dialog; on a `>2ch` device the per-band `Ch` column still appears automatically.

- [ ] **Step 3: Commit (if any fmt/clippy fixes were applied)**

```bash
git add -A && git commit -m "style(gui): fmt + clippy cleanup" || echo "nothing to commit"
```

---

# Part C — Finalization

### Task C1: Full workspace check + docs

**Files:**
- Modify: `CLAUDE.md` (backlog note) — optional
- Modify: `docs/ROADMAP.md` — optional

- [ ] **Step 1: Full workspace gate**

Run: `make check`
Expected: everything green across the whole workspace.

- [ ] **Step 2: Note the feature as shipped (optional)**

If the user wants it recorded, add a one-line entry to `CLAUDE.md`'s "Done & merged" backlog section (lowercase, no AI content), e.g.:
`- advanced-feature visibility toggles (slope/scope/dither/channels) in TUI + GUI settings.`

- [ ] **Step 3: Finish the branch**

Use the `superpowers:finishing-a-development-branch` skill to decide merge vs PR. Do not merge without the user's go-ahead per project convention (verified backend PRs may auto-merge, but this is a UI change — surface it for review).

---

## Notes for the implementer

- **Line numbers drift** as you edit. Always match on the quoted code, not the number.
- **`RoutingMatrix`, `ChannelMask`, `BandScope`** come from `resonance_ipc`; both clients already import what they use at the sites you touch.
- **`slope_db_oct` is `u8`** (`SLOPES: [u8; 3] = [12, 24, 48]`), so `!= 12` compares against the neutral single-biquad slope.
- **Never** add a command that changes DSP state when hiding a feature — the toggles are pure UI. The only DSP command reachable from settings is the pre-existing L/R swap, which the user drives explicitly.
- **egui borrow gotcha** (G2/G4): clone `self.state` before entering a `.show(ctx, |ui| …)` closure that also calls `&mut self` methods, exactly as the existing dialogs do.
