use crate::{
    app::{App, EFFECT_NAMES, InputMode, Panel, fx_enabled, fx_intensity},
    curve,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Bar, BarChart, BarGroup, Block, Borders, Chart, Clear, Dataset, Gauge, GraphType,
        Paragraph, Row, Table, TableState,
    },
};

const DB_RANGE: f64 = 12.0;

pub fn render(app: &App, frame: &mut Frame) {
    let outer = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(5), // spectrum
        Constraint::Length(7),
        Constraint::Length(12),
    ])
    .split(frame.area());

    render_status(app, frame, outer[0]);
    render_eq_curve(app, frame, outer[1]);
    render_spectrum(app, frame, outer[2]);
    render_effects(app, frame, outer[3]);
    render_bands(app, frame, outer[4]);

    if let InputMode::LoadPreset { input } = &app.mode {
        render_load_dialog(input, frame, frame.area());
    }
}

// ── Status bar ────────────────────────────────────────────────────────────

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let power_span = if app.state.as_ref().map(|s| s.enabled).unwrap_or(false) {
        Span::styled("● ON ", Style::default().fg(Color::Green).bold())
    } else {
        Span::styled("○ OFF", Style::default().fg(Color::DarkGray))
    };

    let preset = app
        .state
        .as_ref()
        .and_then(|s| s.current_preset.as_deref())
        .unwrap_or("none");

    let sr = app
        .state
        .as_ref()
        .map(|s| format!("{:.0} Hz", s.sample_rate))
        .unwrap_or_default();

    let preamp = app
        .state
        .as_ref()
        .map(|s| {
            if s.preamp_db.abs() < 0.05 {
                "preamp: 0 dB".to_string()
            } else {
                format!("preamp: {:+.1} dB", s.preamp_db)
            }
        })
        .unwrap_or_default();

    let watch_str = app
        .state
        .as_ref()
        .and_then(|s| s.watched_preset.as_deref())
        .map(|p| {
            let name = std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            format!("  👁 {name}")
        })
        .unwrap_or_default();

    let status_color = if app.status.is_empty() {
        Color::DarkGray
    } else {
        Color::Yellow
    };
    let status_str = if app.status.is_empty() {
        String::new()
    } else {
        format!("  {}", app.status)
    };

    let line = Line::from(vec![
        power_span,
        Span::raw("  "),
        Span::styled(
            format!("preset: {preset}"),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  "),
        Span::styled(sr, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(preamp, Style::default().fg(Color::DarkGray)),
        Span::styled(watch_str, Style::default().fg(Color::Cyan)),
        Span::styled(status_str, Style::default().fg(status_color)),
        Span::raw("  "),
        Span::styled(
            "[p] power  [l] load  [Tab] panel  [←→] adjust  [Space] toggle  [q] quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

// ── EQ curve ──────────────────────────────────────────────────────────────

fn render_eq_curve(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" EQ Frequency Response ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (curve_data, sr) = match &app.state {
        Some(s) if !s.bands.is_empty() => {
            let pts = curve::curve_points(&s.bands, s.sample_rate, 200);
            (pts, s.sample_rate)
        }
        _ => {
            // Flat 0 dB line when disconnected
            let pts = curve::curve_points(&[], 48000.0, 200);
            (pts, 48000.0)
        }
    };

    let dataset = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&curve_data);

    // X-axis: log10(freq) from log10(20) to log10(20000)
    let log_min = curve::x_axis_ticks()[0].0;
    let log_max = curve::x_axis_ticks().last().unwrap().0;

    let x_labels: Vec<ratatui::text::Span> = curve::x_axis_ticks()
        .iter()
        .map(|(_, label)| Span::raw(*label))
        .collect();

    let y_labels = vec![
        Span::styled(
            format!("-{DB_RANGE:.0}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" 0", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("+{DB_RANGE:.0}"),
            Style::default().fg(Color::DarkGray),
        ),
    ];

    // Zero-line dataset
    let zero_pts: Vec<(f64, f64)> = vec![(log_min, 0.0), (log_max, 0.0)];
    let zero_dataset = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::DarkGray))
        .data(&zero_pts);

    let chart = Chart::new(vec![zero_dataset, dataset])
        .x_axis(
            Axis::default()
                .bounds([log_min, log_max])
                .labels(x_labels)
                .style(Style::default().fg(Color::DarkGray)),
        )
        .y_axis(
            Axis::default()
                .bounds([-DB_RANGE, DB_RANGE])
                .labels(y_labels)
                .style(Style::default().fg(Color::DarkGray)),
        );

    frame.render_widget(chart, inner);
    let _ = sr;
}

// ── Spectrum analyzer ────────────────────────────────────────────────────

const BAND_LABELS: [&str; 16] = [
    "25", "40", "63", "100", "160", "250", "400", "630", "1k", "1.6k", "2.5k", "4k", "6.3k", "10k",
    "16k", "20k",
];

fn render_spectrum(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Spectrum ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let bins = app
        .state
        .as_ref()
        .map(|s| s.spectrum.clone())
        .unwrap_or_default();

    if bins.is_empty() {
        return;
    }

    let max_bar = inner.height.saturating_sub(1) as u64;
    let bar_width = (inner.width / bins.len() as u16).max(1);

    let bar_objs: Vec<Bar> = bins
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let label = BAND_LABELS.get(i).copied().unwrap_or("");
            let height = (v * max_bar as f32).round() as u64;
            Bar::default()
                .label(label.into())
                .value(height)
                .style(Style::default().fg(Color::Green))
                .text_value(String::new())
        })
        .collect();

    let group = BarGroup::default().bars(&bar_objs);

    let chart = BarChart::default()
        .bar_width(bar_width)
        .bar_gap(0)
        .label_style(Style::default().fg(Color::DarkGray))
        .data(group)
        .max(max_bar);

    frame.render_widget(chart, inner);
}

// ── FxSound effects panel ─────────────────────────────────────────────────

fn render_effects(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Effects;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" FxSound Effects  [Tab → EQ Bands] ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(inner);

    // 5 effects split: 3 on left, 2 on right
    let effect_rows_left = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(cols[0]);

    let effect_rows_right = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(cols[1]);

    let pairs = [
        (0, effect_rows_left[0]),
        (1, effect_rows_left[1]),
        (2, effect_rows_left[2]),
        (3, effect_rows_right[0]),
        (4, effect_rows_right[1]),
    ];

    for (idx, row_area) in pairs {
        render_effect_row(app, frame, idx, row_area, focused);
    }
}

fn render_effect_row(app: &App, frame: &mut Frame, idx: usize, area: Rect, panel_focused: bool) {
    let selected = panel_focused && app.effect_cursor == idx;
    let name = EFFECT_NAMES[idx];

    let (intensity, enabled) = app
        .state
        .as_ref()
        .map(|s| (fx_intensity(s, idx), fx_enabled(s, idx)))
        .unwrap_or((0.0, false));

    let name_style = if selected {
        Style::default().fg(Color::Yellow).bold()
    } else {
        Style::default().fg(Color::White)
    };

    let enable_sym = if enabled {
        Span::styled("●", Style::default().fg(Color::Green))
    } else {
        Span::styled("○", Style::default().fg(Color::DarkGray))
    };

    let pct = (intensity * 100.0).round() as u16;

    // Layout: name (10) + gauge (remaining - 10) + " NNN% " (6) + enable (2)
    let row = Layout::horizontal([
        Constraint::Length(10),
        Constraint::Min(8),
        Constraint::Length(6),
        Constraint::Length(2),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Span::styled(name, name_style)), row[0]);

    let gauge_color = if !enabled {
        Color::DarkGray
    } else if selected {
        Color::Yellow
    } else {
        Color::Cyan
    };

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
        .percent(pct)
        .label("");
    frame.render_widget(gauge, row[1]);

    frame.render_widget(
        Paragraph::new(format!(" {:3}%", pct)).style(Style::default().fg(Color::Gray)),
        row[2],
    );

    frame.render_widget(Paragraph::new(Line::from(vec![enable_sym])), row[3]);
}

// ── EQ bands panel ────────────────────────────────────────────────────────

fn render_bands(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Bands;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" EQ Bands  [Tab → Effects] ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let bands = match &app.state {
        Some(s) if !s.bands.is_empty() => s.bands.clone(),
        _ => {
            frame.render_widget(
                Paragraph::new(" (no bands — not connected or no preset loaded)")
                    .style(Style::default().fg(Color::DarkGray)),
                inner,
            );
            return;
        }
    };

    let header = Row::new(["#", "Freq", "Gain", "Q", "En"])
        .style(Style::default().bold().fg(Color::DarkGray))
        .height(1);

    let rows: Vec<Row> = bands
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let selected = focused && app.band_cursor == i;
            let row_style = if selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::White)
            };

            let freq_str = fmt_freq(b.freq);
            let gain_str = format!("{:+.1}", b.gain_db);
            let q_str = format!("{:.2}", b.q);
            let en_str = if b.enabled { "●" } else { "○" };

            Row::new(vec![
                format!("{}", i + 1),
                freq_str,
                gain_str,
                q_str,
                en_str.to_string(),
            ])
            .style(row_style)
        })
        .collect();

    let mut table_state =
        TableState::default().with_selected(if focused { Some(app.band_cursor) } else { None });

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(3),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .column_spacing(1);

    frame.render_stateful_widget(table, inner, &mut table_state);
}

// ── Load-preset dialog ────────────────────────────────────────────────────

fn render_load_dialog(input: &str, frame: &mut Frame, area: Rect) {
    let dialog_w = 60u16;
    let dialog_h = 4u16;
    let x = area.x + area.width.saturating_sub(dialog_w) / 2;
    let y = area.y + area.height.saturating_sub(dialog_h) / 2;
    let dialog_area = Rect::new(x, y, dialog_w.min(area.width), dialog_h.min(area.height));

    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(" Load Preset ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let inner_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    frame.render_widget(
        Paragraph::new("Path (.fac or APO .txt):").style(Style::default().fg(Color::DarkGray)),
        inner_rows[0],
    );

    let cursor_style = Style::default().fg(Color::Yellow);
    frame.render_widget(
        Paragraph::new(format!("{input}▋")).style(cursor_style),
        inner_rows[1],
    );
}

// ── Format helpers ────────────────────────────────────────────────────────

fn fmt_freq(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{:.1}kHz", hz / 1000.0)
    } else {
        format!("{:.0}Hz", hz)
    }
}
