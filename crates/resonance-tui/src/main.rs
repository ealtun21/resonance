mod app;
mod browser;
mod curve;
mod layout;
mod ui;

use anyhow::Result;
use app::{App, InputMode};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use std::time::{Duration, Instant};

const DEFAULT_FPS: u64 = 30;
const MIN_FPS: u64 = 5;
const MAX_FPS: u64 = 144;

fn main() -> Result<()> {
    let fps = parse_fps();
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stderr(), EnableMouseCapture)?;
    let result = run(&mut terminal, fps);
    crossterm::execute!(std::io::stderr(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

/// Resolve the refresh rate from `--fps N` / `-f N`, then `RESONANCE_FPS`,
/// then the default. Clamped to [MIN_FPS, MAX_FPS].
fn parse_fps() -> u64 {
    let clamp = |n: u64| n.clamp(MIN_FPS, MAX_FPS);

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--fps" || a == "-f" {
            if let Some(n) = args.next().and_then(|v| v.parse::<u64>().ok()) {
                return clamp(n);
            }
        } else if let Some(v) = a.strip_prefix("--fps=") {
            if let Ok(n) = v.parse::<u64>() {
                return clamp(n);
            }
        }
    }
    if let Ok(n) = std::env::var("RESONANCE_FPS")
        .unwrap_or_default()
        .parse::<u64>()
    {
        return clamp(n);
    }
    DEFAULT_FPS
}

fn run(terminal: &mut ratatui::DefaultTerminal, fps: u64) -> Result<()> {
    let mut app = App::new();
    app.connect();
    app.refresh_state();

    let refresh = Duration::from_millis(1000 / fps);
    // Keep input responsive even at low fps by polling at least every 33 ms.
    let poll_timeout = refresh.min(Duration::from_millis(33));
    let mut last_refresh = Instant::now();

    while app.running {
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

    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    // Quit with q or Ctrl+C regardless of mode
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
    }
}

fn handle_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Tab => app.next_panel(),
        KeyCode::Char('p') => app.toggle_power(),
        KeyCode::Char('l') => app.begin_load_preset(),
        KeyCode::Up => app.cursor_up(),
        KeyCode::Down => app.cursor_down(),
        KeyCode::Right => {
            let delta = if key.modifiers.contains(KeyModifiers::SHIFT) {
                0.10
            } else {
                0.05
            };
            app.adjust(delta);
        }
        KeyCode::Left => {
            let delta = if key.modifiers.contains(KeyModifiers::SHIFT) {
                0.10
            } else {
                0.05
            };
            app.adjust(-delta);
        }
        KeyCode::Char('+') | KeyCode::Char('=') => app.preamp_adjust(0.5),
        KeyCode::Char('-') => app.preamp_adjust(-0.5),
        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_selected(),
        KeyCode::Char('a') if app.focus == app::Panel::Bands => app.add_band(),
        KeyCode::Char('d') | KeyCode::Delete if app.focus == app::Panel::Bands => app.remove_band(),
        KeyCode::Char('t') if app.focus == app::Panel::Bands => app.cycle_band_type(),
        _ => {}
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.mouse_click(mouse.column, mouse.row);
        }
        MouseEventKind::ScrollUp => {
            app.mouse_scroll(mouse.column, mouse.row, 0.05);
        }
        MouseEventKind::ScrollDown => {
            app.mouse_scroll(mouse.column, mouse.row, -0.05);
        }
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
        _ => {}
    }
}
