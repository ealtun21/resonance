mod app;
mod curve;
mod ui;

use anyhow::Result;
use app::{App, InputMode};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let mut app = App::new();
    app.connect();
    app.refresh_state();

    let mut last_refresh = Instant::now();

    while app.running {
        terminal.draw(|f| ui::render(&app, f))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                handle_key(&mut app, key);
            }
        }

        if last_refresh.elapsed() >= POLL_INTERVAL {
            app.refresh_state();
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    // Quit with q or Ctrl+C regardless of mode
    if key.code == KeyCode::Char('q') && key.modifiers.is_empty() && app.mode == InputMode::Normal {
        app.running = false;
        return;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.running = false;
        return;
    }

    match &app.mode {
        InputMode::Normal => handle_normal(app, key),
        InputMode::LoadPreset { .. } => handle_load_preset(app, key),
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
        // Preamp with +/-
        KeyCode::Char('+') | KeyCode::Char('=') => app.preamp_adjust(0.5),
        KeyCode::Char('-') => app.preamp_adjust(-0.5),
        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_selected(),
        _ => {}
    }
}

fn handle_load_preset(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Enter => app.confirm_load_preset(),
        KeyCode::Backspace => app.pop_input_char(),
        KeyCode::Char(c) => app.push_input_char(c),
        _ => {}
    }
}
