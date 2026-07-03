mod app;
mod browser;
mod curve;
mod layout;
mod prefs;
mod settings;
mod ui;

use anyhow::Result;
use app::{App, InputMode};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use std::time::{Duration, Instant};

const MIN_FPS: u64 = 5;
const MAX_FPS: u64 = 240;
const MIN_REFRESH_MS: u64 = 16;
const MAX_REFRESH_MS: u64 = 2000;

fn main() -> Result<()> {
    // `--fps`/`RESONANCE_FPS` override the saved preference when present.
    let fps_override = parse_fps_override();
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stderr(), EnableMouseCapture)?;
    // ratatui's panic hook restores raw mode / the alternate screen but does not
    // know about the mouse capture we just enabled — disable it on panic too, or
    // the user's terminal keeps emitting mouse escape codes after a crash.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stderr(), DisableMouseCapture);
        prev_hook(info);
    }));
    let result = run(&mut terminal, fps_override);
    crossterm::execute!(std::io::stderr(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

/// An explicit frame-rate override from `--fps N` / `-f N` / `--fps=N`, then
/// `RESONANCE_FPS`. `None` if unset — the saved preference is used instead.
fn parse_fps_override() -> Option<u64> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--fps" || a == "-f" {
            if let Some(n) = args.next().and_then(|v| v.parse::<u64>().ok()) {
                return Some(n);
            }
        } else if let Some(v) = a.strip_prefix("--fps=") {
            if let Ok(n) = v.parse::<u64>() {
                return Some(n);
            }
        }
    }
    std::env::var("RESONANCE_FPS").ok()?.parse::<u64>().ok()
}

fn run(terminal: &mut ratatui::DefaultTerminal, fps_override: Option<u64>) -> Result<()> {
    let mut app = App::new();
    app.connect();
    app.refresh_state();

    let mut last_refresh = Instant::now();

    while app.running {
        // Read render rate and daemon-poll interval from prefs each iteration so
        // a change in the settings UI takes effect live. A CLI/env override wins
        // over the saved fps. Polling is decoupled from rendering: the UI redraws
        // at `fps`, but state is fetched only every `refresh_ms`.
        let fps = fps_override
            .unwrap_or(app.prefs.fps)
            .clamp(MIN_FPS, MAX_FPS);
        let frame = Duration::from_millis(1000 / fps);
        let refresh =
            Duration::from_millis(app.prefs.refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS));
        let poll_timeout = frame.min(Duration::from_millis(33));

        app.animate_spectrum();
        app.pump_autoeq();
        app.pump_downloads();
        let mut frame_area = app.last_frame;
        terminal.draw(|f| {
            frame_area = f.area();
            ui::render(&app, f);
        })?;
        app.last_frame = frame_area;

        if event::poll(poll_timeout)? {
            match event::read()? {
                Event::Key(key) => handle_key(&mut app, key),
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                _ => {}
            }
        }

        if last_refresh.elapsed() >= refresh {
            app.refresh_state();
            last_refresh = Instant::now();
        }
    }

    // Persist the reference overlay so a loaded measurement/target survives.
    app.save_reference();
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Char('q') && key.modifiers.is_empty() && app.mode.is_normal() {
        app.running = false;
        return;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.running = false;
        return;
    }

    match &app.mode {
        InputMode::Normal => handle_normal(app, key),
        InputMode::Browse(_) => handle_browse(app, key),
        InputMode::SelectOutput { .. } => handle_select_output(app, key),
        InputMode::Settings(_) => handle_settings(app, key),
        InputMode::SelectBandChannels { .. } => handle_band_channels(app, key),
        InputMode::EditBandDynamics { .. } => handle_band_dynamics(app, key),
        InputMode::SquigBrowse { .. } => handle_squig_browse(app, key),
        // Any key dismisses the help overlay.
        InputMode::Help => app.cancel_input(),
    }
}

fn handle_normal(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('z') => {
                app.undo();
                return;
            }
            KeyCode::Char('y') => {
                app.redo();
                return;
            }
            _ => {}
        }
    }
    // Band-editing keys work from either the Bands table or the Graph.
    let band_focus = matches!(app.focus, app::Panel::Bands | app::Panel::Graph);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Tab => app.next_panel(),
        KeyCode::Char('p') => app.toggle_power(),
        KeyCode::Char('l') => app.begin_load_preset(),
        KeyCode::Char('s') => app.begin_settings(),
        // ── Graph panel: arrows drag the selected node in 2 axes ──
        KeyCode::Up if app.focus == app::Panel::Graph => {
            app.graph_nudge(if shift { 1.0 } else { 0.5 }, 0.0);
        }
        KeyCode::Down if app.focus == app::Panel::Graph => {
            app.graph_nudge(if shift { -1.0 } else { -0.5 }, 0.0);
        }
        KeyCode::Left if app.focus == app::Panel::Graph => {
            app.graph_nudge(0.0, if shift { -2.0 } else { -1.0 });
        }
        KeyCode::Right if app.focus == app::Panel::Graph => {
            app.graph_nudge(0.0, if shift { 2.0 } else { 1.0 });
        }
        KeyCode::Char('[') if app.focus == app::Panel::Graph => app.graph_select(-1),
        KeyCode::Char(']') if app.focus == app::Panel::Graph => app.graph_select(1),
        // ── Effects / Bands: select + adjust active field ──
        KeyCode::Up => app.cursor_up(),
        KeyCode::Down => app.cursor_down(),
        KeyCode::Right => app.adjust(if shift { 0.10 } else { 0.05 }),
        KeyCode::Left => app.adjust(if shift { -0.10 } else { -0.05 }),
        KeyCode::Char('+' | '=') => app.preamp_adjust(0.5),
        KeyCode::Char('-') => app.preamp_adjust(-0.5),
        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_selected(),
        KeyCode::Char('a') if band_focus => app.add_band(),
        KeyCode::Char('d') | KeyCode::Delete if band_focus => app.remove_band(),
        KeyCode::Char('t') if band_focus => app.cycle_band_type(),
        // Advanced keys are gated behind their Settings → Preferences toggle so
        // a clean default UI has no hidden shortcuts; when off they no-op.
        // Cycle the selected band's filter slope 12→24→48 (shelves + HP/LP only;
        // gated inside the call). Uppercase so lowercase `s` keeps settings.
        KeyCode::Char('S') if band_focus && app.prefs.show_slope => app.cycle_band_slope(),
        // Cycle the selected band's stereo scope Stereo→Mid→Side (all band types;
        // audible on ≥2ch). Uppercase, matching the `S` slope-cycle convention.
        KeyCode::Char('M') if band_focus && app.prefs.show_scope => app.cycle_band_scope(),
        // Dynamic EQ on the selected band (peaking only; gated inside the call):
        // lowercase toggles on/off with defaults, uppercase opens the parameter
        // editor. Plain `y` is free — redo needs Ctrl.
        // Solo (audition) the selected band — bypass every other band. Transient
        // (no undo/save). Uppercase `L` (Listen) so lowercase `l` keeps its
        // load-preset meaning; no pref gate (it's a transient action on any band).
        KeyCode::Char('L') if band_focus => app.toggle_band_solo(),
        KeyCode::Char('y') if band_focus && app.prefs.show_dynamics => app.toggle_band_dynamics(),
        KeyCode::Char('Y') if band_focus && app.prefs.show_dynamics => {
            app.begin_edit_band_dynamics();
        }
        // Per-band channel targeting (channels visible only).
        KeyCode::Char('c') if band_focus && app.show_ch() => app.begin_select_band_channels(),
        // L/R channel swap (channels visible only; also reachable from settings).
        KeyCode::Char('w') if app.show_ch() => app.toggle_swap_lr(),
        KeyCode::Char('o') => app.begin_select_output(),
        // Toggle the Applications / Outputs volume panels (uppercase, so lowercase
        // `a`/`o` keep their band-add / output-selector meanings).
        KeyCode::Char('A') => app.toggle_apps_panel(),
        KeyCode::Char('O') => app.toggle_sinks_panel(),
        // Cycle output dither depth (uppercase, so lowercase `d` keeps its
        // delete-band meaning): Off → 16 → 20 → 24 → Off.
        KeyCode::Char('D') if app.prefs.show_dither => app.cycle_dither(),
        // Convolution impulse-response picker (uppercase, matching the other
        // advanced keys). Enter loads/replaces; `t` bypasses, `x` removes.
        KeyCode::Char('I') if app.prefs.show_ir => app.open_ir_browser(),
        KeyCode::Char('?') => app.show_help(),
        _ => {}
    }
}

/// squig.link online browser: type to search, ↑↓ move, Enter loads the
/// selected entry, Tab switches measurements/targets, Ctrl-R refreshes.
fn handle_squig_browse(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.code == KeyCode::Char('r') {
            app.squig_refresh();
        }
        return;
    }
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Up => app.squig_move(-1),
        KeyCode::Down => app.squig_move(1),
        KeyCode::PageUp => app.squig_move(-10),
        KeyCode::PageDown => app.squig_move(10),
        KeyCode::Tab => app.squig_switch_tab(),
        KeyCode::F(5) => app.squig_refresh(),
        KeyCode::Enter => app.squig_enter(),
        KeyCode::Backspace => app.squig_backspace(),
        // Everything printable types into the search box (Alt-modified ignored).
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => app.squig_query_char(c),
        _ => {}
    }
}

/// Per-band dynamics editor: ↑↓/jk pick a parameter, ←→ step it (Shift = ×5),
/// Enter applies, Esc cancels.
fn handle_band_dynamics(app: &mut App, key: KeyEvent) {
    let steps = if key.modifiers.contains(KeyModifiers::SHIFT) {
        5.0
    } else {
        1.0
    };
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Up | KeyCode::Char('k') => app.band_dynamics_move(-1),
        KeyCode::Down | KeyCode::Char('j') => app.band_dynamics_move(1),
        KeyCode::Left => app.band_dynamics_adjust(-steps),
        KeyCode::Right => app.band_dynamics_adjust(steps),
        KeyCode::Enter => app.band_dynamics_apply(),
        _ => {}
    }
}

/// Per-band channel-target picker: ↑↓/jk move, Space toggles the channel under
/// the cursor, `a`/`n` select all/none, Enter applies, Esc cancels.
fn handle_band_channels(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Up | KeyCode::Char('k') => app.band_channels_move(-1),
        KeyCode::Down | KeyCode::Char('j') => app.band_channels_move(1),
        KeyCode::Char(' ') => app.band_channels_toggle(),
        KeyCode::Char('a') => app.band_channels_set_all(),
        KeyCode::Char('n') => app.band_channels_set_none(),
        KeyCode::Enter => app.band_channels_apply(),
        _ => {}
    }
}

fn handle_select_output(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Up | KeyCode::Char('k') => app.output_move(-1),
        KeyCode::Down | KeyCode::Char('j') => app.output_move(1),
        KeyCode::Enter => app.output_enter(),
        _ => {}
    }
}

fn handle_settings(app: &mut App, key: KeyEvent) {
    // Text input is active — route all keystrokes to it.
    if app.settings_has_text_input() {
        match key.code {
            KeyCode::Esc => app.settings_cancel_text(),
            KeyCode::Enter => app.settings_confirm_text(),
            KeyCode::Left => app.settings_cursor_left(),
            KeyCode::Right => app.settings_cursor_right(),
            KeyCode::Backspace => app.settings_backspace(),
            // Skip chords (Ctrl-/Alt-modified): they would otherwise insert the
            // bare character into the field instead of being ignored.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                app.settings_text_char(c);
            }
            _ => {}
        }
        return;
    }

    // Confirm dialog is active.
    if app.settings_has_confirm() {
        match key.code {
            KeyCode::Char('y') => app.settings_confirm_yes(),
            KeyCode::Char('n') | KeyCode::Esc => app.settings_confirm_no(),
            _ => {}
        }
        return;
    }

    // Profile sub-picker is active.
    if app.settings_has_sub_picker() {
        match key.code {
            KeyCode::Esc => app.settings_close_sub_picker(),
            KeyCode::Up | KeyCode::Char('k') => app.settings_move(-1),
            KeyCode::Down | KeyCode::Char('j') => app.settings_move(1),
            KeyCode::Enter => app.settings_sub_picker_confirm(),
            _ => {}
        }
        return;
    }

    // Normal settings navigation.
    match key.code {
        KeyCode::Esc => app.settings_close(),
        // Tab/BackTab and ←/→ both switch tabs (arrow-only navigation).
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => app.settings_tab_shift(1),
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => app.settings_tab_shift(-1),
        KeyCode::Char('1') => app.settings_set_tab(0),
        KeyCode::Char('2') => app.settings_set_tab(1),
        KeyCode::Char('3') => app.settings_set_tab(2),
        KeyCode::Char('4') => app.settings_set_tab(3),
        KeyCode::Char('5') => app.settings_set_tab(4),
        KeyCode::Char('6') => app.settings_set_tab(5),
        KeyCode::Up | KeyCode::Char('k') => app.settings_move(-1),
        KeyCode::Down | KeyCode::Char('j') => app.settings_move(1),
        KeyCode::Enter | KeyCode::Char(' ') => app.settings_enter(),
        // Adjust the customizer value under the cursor (Reference tab).
        KeyCode::Char('+' | '=') => app.settings_adjust(1.0),
        KeyCode::Char('-') => app.settings_adjust(-1.0),
        KeyCode::Char('n') => app.settings_key_n(),
        KeyCode::Char('e') => app.settings_key_e(),
        KeyCode::Char('r') => app.settings_key_r(),
        KeyCode::Char('d') | KeyCode::Delete => app.settings_key_d(),
        KeyCode::Char('m') => app.settings_key_m(),
        _ => {}
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    // The squig browser is a long scrollable list, so allow the wheel to move
    // its cursor even though it's a modal (clicks still ignored).
    if matches!(app.mode, InputMode::SquigBrowse { .. }) {
        match mouse.kind {
            MouseEventKind::ScrollUp => app.squig_move(-1),
            MouseEventKind::ScrollDown => app.squig_move(1),
            _ => {}
        }
        return;
    }
    // Mouse targets are the main-screen panels only; while any other modal
    // (browser, settings, output picker, help) is open, ignore clicks/scrolls
    // so they can't mutate EQ state hidden behind the dialog.
    if !app.mode.is_normal() {
        return;
    }
    let (col, row) = (mouse.column, mouse.row);

    // FR-graph node editing: left-press grabs the nearest node (freq+gain drag),
    // right-press tunes Q, wheel nudges gain. A drag in progress keeps receiving
    // moves/release even if the cursor leaves the panel.
    let on_graph = app.in_eq_panel(col, row);
    if on_graph || app.is_graph_dragging() {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if on_graph => {
                app.graph_press(col, row, false);
                return;
            }
            MouseEventKind::Down(MouseButton::Right) if on_graph => {
                app.graph_press(col, row, true);
                return;
            }
            MouseEventKind::Drag(_) if app.is_graph_dragging() => {
                app.graph_drag_to(col, row);
                return;
            }
            MouseEventKind::Up(_) if app.is_graph_dragging() => {
                app.graph_release();
                return;
            }
            MouseEventKind::ScrollUp if on_graph => {
                app.graph_scroll(col, row, 0.5);
                return;
            }
            MouseEventKind::ScrollDown if on_graph => {
                app.graph_scroll(col, row, -0.5);
                return;
            }
            _ => {}
        }
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => app.mouse_click(col, row),
        MouseEventKind::ScrollUp => app.mouse_scroll(col, row, 0.05),
        MouseEventKind::ScrollDown => app.mouse_scroll(col, row, -0.05),
        _ => {}
    }
}

fn handle_browse(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Up | KeyCode::Char('k') => app.browse_move(-1),
        KeyCode::Down | KeyCode::Char('j') => app.browse_move(1),
        KeyCode::PageUp => app.browse_move(-10),
        KeyCode::PageDown => app.browse_move(10),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => app.browse_enter(),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => app.browse_parent(),
        // IR-picker extras (no-ops for other browse purposes): toggle bypass /
        // remove the loaded impulse response without picking a file.
        KeyCode::Char('t') => app.browse_ir_toggle(),
        KeyCode::Char('x') => app.browse_ir_clear(),
        _ => {}
    }
}
