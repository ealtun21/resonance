use crate::{
    app::{App, BandField, EFFECT_NAMES, InputMode, Panel, fx_enabled, fx_intensity, fx_min},
    browser::Browser,
    curve,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, Gauge, GraphType, List, ListItem, ListState,
        Paragraph,
    },
};

const DB_RANGE: f64 = 18.0;

pub fn render(app: &App, frame: &mut Frame) {
    let p = crate::layout::panes(frame.area());

    render_status(app, frame, p.status);
    render_eq_curve(app, frame, p.eq);
    render_spectrum(app, frame, p.spectrum);
    render_effects(app, frame, p.effects);
    render_bands(app, frame, p.bands);
    render_footer(app, frame, p.footer);

    if let InputMode::Browse(b) = &app.mode {
        render_browser(b, frame, frame.area());
    }
}

// ── Footer / contextual help ───────────────────────────────────────────────

fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let common = "[Tab] focus  [↑↓] select  [←→] adjust  [+/-] preamp  [Space] toggle  [l] load  [p] power  [q] quit";
    let ctx = match app.focus {
        Panel::Effects => "  •  [←→] intensity",
        Panel::Bands => "  •  [a] add  [d] del  [t] type",
    };
    let line = Line::from(vec![
        Span::styled(format!(" {common}"), Style::default().fg(Color::DarkGray)),
        Span::styled(ctx, Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
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

    let preamp_db = app.state.as_ref().map(|s| s.preamp_db).unwrap_or(0.0);
    let preamp = if preamp_db.abs() < 0.05 {
        "preamp 0 dB".to_string()
    } else {
        format!("preamp {preamp_db:+.1} dB")
    };
    let preamp_color = if preamp_db.abs() < 0.05 {
        Color::DarkGray
    } else {
        Color::Cyan
    };

    let output_str = app
        .state
        .as_ref()
        .and_then(|s| s.active_output.as_deref())
        .map(|o| format!("  🔊 {o}"))
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

    let sep = || Span::styled(" │ ", Style::default().fg(Color::DarkGray));

    let line = Line::from(vec![
        Span::styled(" ♪ Resonance ", Style::default().fg(Color::Magenta).bold()),
        power_span,
        sep(),
        Span::styled(format!("preset {preset}"), Style::default().fg(Color::Cyan)),
        sep(),
        Span::styled(sr, Style::default().fg(Color::DarkGray)),
        sep(),
        Span::styled(preamp, Style::default().fg(preamp_color)),
        Span::styled(output_str, Style::default().fg(Color::Cyan)),
        Span::styled(status_str, Style::default().fg(status_color)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

// ── EQ curve ──────────────────────────────────────────────────────────────

fn render_eq_curve(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Bands;
    let block = Block::default()
        .title(Line::from(" EQ Frequency Response ").fg(Color::Magenta))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (bands, sr) = match &app.state {
        Some(s) if !s.bands.is_empty() => (s.bands.clone(), s.sample_rate),
        _ => (Vec::new(), 48000.0),
    };

    // Resolve to roughly the braille sub-column count so narrow peaks render.
    let n_points = (inner.width as usize * 2).clamp(240, 1600);
    let curve_data = curve::curve_points(&bands, sr, n_points);

    let log_min = curve::x_axis_ticks()[0].0;
    let log_max = curve::x_axis_ticks().last().unwrap().0;

    // Band markers: a dot at (log10(freq), gain) for each enabled band.
    let sel = if focused { app.band_cursor } else { usize::MAX };
    let markers_other: Vec<(f64, f64)> = bands
        .iter()
        .enumerate()
        .filter(|(i, b)| b.enabled && *i != sel)
        .map(|(_, b)| {
            (
                curve::band_marker_x(b.freq),
                b.gain_db.clamp(-DB_RANGE, DB_RANGE),
            )
        })
        .collect();
    let marker_sel: Vec<(f64, f64)> = bands
        .get(sel)
        .filter(|b| b.enabled)
        .map(|b| {
            (
                curve::band_marker_x(b.freq),
                b.gain_db.clamp(-DB_RANGE, DB_RANGE),
            )
        })
        .into_iter()
        .collect();

    let zero_pts: Vec<(f64, f64)> = vec![(log_min, 0.0), (log_max, 0.0)];

    let x_labels: Vec<Span> = curve::x_axis_ticks()
        .iter()
        .map(|(_, label)| Span::styled(*label, Style::default().fg(Color::DarkGray)))
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

    let datasets = vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::DarkGray))
            .data(&zero_pts),
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&curve_data),
        Dataset::default()
            .marker(symbols::Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(Color::DarkGray))
            .data(&markers_other),
        Dataset::default()
            .marker(symbols::Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(Color::Yellow).bold())
            .data(&marker_sel),
    ];

    let chart = Chart::new(datasets)
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
}

// ── Spectrum analyzer ────────────────────────────────────────────────────

/// Colour gradient by bar-height fraction: green (low) → yellow → red (peaks).
fn spectrum_color(t: f64) -> Color {
    if t < 0.45 {
        Color::Green
    } else if t < 0.75 {
        Color::LightGreen
    } else if t < 0.9 {
        Color::Yellow
    } else {
        Color::LightRed
    }
}

/// Mirrored spectrum: an interpolated silhouette grows up and down from a
/// centre baseline, with 8-level partial blocks for smooth sub-row height.
fn render_spectrum(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(Line::from(" Spectrum ").fg(Color::Magenta))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height < 2 {
        return;
    }

    let bins = app.state.as_ref().map(|s| s.spectrum.as_slice());
    let Some(bins) = bins.filter(|b| !b.is_empty()) else {
        return;
    };
    let rows = inner.height / 2; // whole rows available each direction
    if rows < 1 {
        return;
    }
    let half = rows as f64;
    let center_up = inner.y + inner.height / 2; // first row of the lower half
    let n = bins.len();
    let buf = frame.buffer_mut();

    // 8-level partial blocks (fill from bottom of a cell — used for upward bars).
    const LOWER: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    // Linearly interpolate the bins across the full width → smooth silhouette
    // where every column gets its own height (no flat, blocky plateaus).
    let w = inner.width;
    let sample = |data: &[f32], cx: u16| -> f64 {
        if w <= 1 {
            return data[0].clamp(0.0, 1.0) as f64;
        }
        let pos = cx as f64 / (w - 1) as f64 * (n - 1) as f64;
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(n - 1);
        let frac = pos - i0 as f64;
        let a = data[i0].clamp(0.0, 1.0) as f64;
        let b = data[i1].clamp(0.0, 1.0) as f64;
        a + (b - a) * frac
    };

    let bottom = inner.y + inner.height;
    for cx in 0..inner.width {
        let x = inner.x + cx;
        let v = sample(bins, cx);

        // Always at least 1/8 tall → a thin centre line that grows, never an
        // empty gap. Height in eighths of a cell → full cells + fractional tip.
        let eighths = ((v * half * 8.0).round() as u16).max(1);
        let full = (eighths / 8).min(rows);
        let rem = (eighths % 8) as usize;
        let idle = eighths <= 1;

        for r in 0..full {
            let color = spectrum_color(r as f64 / half);
            let yu = center_up - 1 - r;
            buf[(x, yu)].set_char('█').set_fg(color);
            let yd = center_up + r;
            if yd < bottom {
                buf[(x, yd)].set_char('█').set_fg(color);
            }
        }
        if rem > 0 && full < rows {
            let color = if idle {
                Color::DarkGray // baseline tint when there's no signal
            } else {
                spectrum_color(full as f64 / half)
            };
            // Up tip: lower block fills from the cell bottom (continuous upward).
            let yu = center_up - 1 - full;
            buf[(x, yu)].set_char(LOWER[rem]).set_fg(color);
            // Down tip: approximate an "upper" partial so the fill stays continuous.
            let yd = center_up + full;
            if yd < bottom {
                let ch = if rem >= 4 { '▀' } else { '▔' };
                buf[(x, yd)].set_char(ch).set_fg(color);
            }
        }
    }
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
        .title(Line::from(" Effects ").fg(if focused { Color::Cyan } else { Color::Magenta }))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // All effects stacked vertically, filling the column.
    let rows = crate::layout::effect_rows(inner, EFFECT_NAMES.len());
    for (idx, block_rect) in rows.iter().enumerate() {
        // Vertically centre the single content line within its block.
        let y = block_rect.y + block_rect.height / 2;
        let line = Rect::new(block_rect.x, y, block_rect.width, 1);
        render_effect_row(app, frame, idx, line, focused);
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

    let bipolar = fx_min(idx) < 0.0;

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

    let pct = (intensity * 100.0).round() as i32;
    let label = if bipolar {
        format!("{pct:+4}%")
    } else {
        format!("{pct:4}%")
    };
    // Fill by magnitude from empty: 0 → empty, ±100% → full. Sign is shown by
    // the gauge colour and the signed label (bipolar effects), not a half-full bar.
    let ratio = intensity.abs().clamp(0.0, 1.0);

    // Layout: name (10) + gauge (remaining) + " NNNN% " (6) + gap (1) + enable (1)
    let row = Layout::horizontal([
        Constraint::Length(10),
        Constraint::Min(6),
        Constraint::Length(6),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Span::styled(name, name_style)), row[0]);

    let gauge_color = if !enabled {
        Color::DarkGray
    } else if intensity < 0.0 {
        Color::LightMagenta
    } else if selected {
        Color::Yellow
    } else {
        Color::Cyan
    };

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
        .ratio(ratio)
        .label("");
    frame.render_widget(gauge, row[1]);

    frame.render_widget(
        Paragraph::new(format!(" {label}")).style(Style::default().fg(Color::Gray)),
        row[2],
    );

    // row[3] is an intentional gap between the % and the enable indicator.
    frame.render_widget(Paragraph::new(Line::from(vec![enable_sym])), row[4]);
}

// ── EQ bands panel ────────────────────────────────────────────────────────

fn render_bands(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Bands;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let field_hint = if focused {
        match app.band_field {
            BandField::Freq => " ▸ Freq",
            BandField::Gain => " ▸ Gain",
            BandField::Q => " ▸ Q",
        }
    } else {
        ""
    };
    let _ = field_hint;
    let block = Block::default()
        .title(Line::from(" EQ Bands ").fg(if focused { Color::Cyan } else { Color::Magenta }))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let bands = match &app.state {
        Some(s) if !s.bands.is_empty() => s.bands.clone(),
        _ => {
            frame.render_widget(
                Paragraph::new(" (no bands — press 'a' to add, or load a preset)")
                    .style(Style::default().fg(Color::DarkGray)),
                inner,
            );
            return;
        }
    };

    if inner.height < 2 {
        return;
    }

    // ── Header row ──
    let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let hcols = crate::layout::band_columns(header_rect);
    let headers = ["#", "Type", "Freq", "Gain", "Q", "En"];
    let hdr_style = Style::default().bold().fg(Color::DarkGray);
    for (c, h) in headers.iter().enumerate() {
        // Highlight the active field's column header when focused.
        let active = focused
            && matches!(
                (c, app.band_field),
                (2, BandField::Freq) | (3, BandField::Gain) | (4, BandField::Q)
            );
        let style = if active {
            Style::default().bold().fg(Color::Yellow)
        } else {
            hdr_style
        };
        frame.render_widget(Paragraph::new(*h).style(style), hcols[c]);
    }

    // ── Data rows (with scroll) ──
    let visible = (inner.height - 1) as usize;
    let offset = crate::layout::band_scroll_offset(app.band_cursor, bands.len(), visible);
    let full_names = crate::layout::band_type_full(inner.width);

    for (vis, i) in (offset..bands.len()).take(visible).enumerate() {
        let b = &bands[i];
        let y = inner.y + 1 + vis as u16;
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let cols = crate::layout::band_columns(row_rect);
        let selected = focused && app.band_cursor == i;

        let type_name = if full_names {
            b.band_type.full().to_string()
        } else {
            b.band_type.abbrev().to_string()
        };
        let cells = [
            format!("{}", i + 1),
            type_name,
            fmt_freq(b.freq),
            format!("{:+.1}", b.gain_db),
            format!("{:.2}", b.q),
            (if b.enabled { "●" } else { "○" }).to_string(),
        ];

        for (c, text) in cells.iter().enumerate() {
            let field_active = selected
                && matches!(
                    (c, app.band_field),
                    (2, BandField::Freq) | (3, BandField::Gain) | (4, BandField::Q)
                );

            // Active field gets a strong highlight; disabled bands grey out;
            // otherwise colour Freq/Gain by meaning.
            let style = if field_active {
                Style::default().fg(Color::Black).bg(Color::Yellow).bold()
            } else if !b.enabled {
                Style::default().fg(Color::DarkGray)
            } else {
                let fg = match c {
                    1 => Color::Cyan,           // type
                    2 => freq_color(b.freq),    // freq across the spectrum
                    3 => gain_color(b.gain_db), // green up / red down
                    5 => Color::Green,          // enabled dot
                    _ => Color::Gray,           // # and Q
                };
                let s = Style::default().fg(fg);
                if selected { s.bold() } else { s }
            };
            frame.render_widget(Paragraph::new(text.as_str()).style(style), cols[c]);
        }
    }
}

// ── Band cell colour mapping ───────────────────────────────────────────────

/// Gain: neutral grey at 0, deepening green for boosts, deepening red for cuts.
fn gain_color(db: f64) -> Color {
    if db.abs() < 0.1 {
        return Color::Gray;
    }
    let t = (db.abs() / 15.0).clamp(0.0, 1.0);
    let lvl = (130.0 + 125.0 * t) as u8;
    if db > 0.0 {
        Color::Rgb(40, lvl, 40)
    } else {
        Color::Rgb(lvl, 45, 45)
    }
}

/// Frequency mapped across the colour spectrum: low = red, mid = green,
/// high = blue/violet (hue 0 → 280° over 20 Hz–20 kHz on a log scale).
fn freq_color(freq: f64) -> Color {
    let t = ((freq.clamp(20.0, 20000.0).log10() - 20f64.log10()) / 3.0).clamp(0.0, 1.0);
    let (r, g, b) = hsv_to_rgb(280.0 * t, 0.65, 1.0);
    Color::Rgb(r, g, b)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let c = v * s;
    let hp = (h / 60.0).rem_euclid(6.0);
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

// ── Load-preset file picker ─────────────────────────────────────────────────

fn centered_rect(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn render_browser(b: &Browser, frame: &mut Frame, area: Rect) {
    let dialog = centered_rect(area, 80, 80);
    frame.render_widget(Clear, dialog);

    let cwd = b.cwd.display().to_string();
    let block = Block::default()
        .title(Line::from(format!(" Load Preset — {cwd} ")).fg(Color::Yellow))
        .title_bottom(
            Line::from(" ↑↓ move   →/Enter open   ← back   Esc cancel ").fg(Color::DarkGray),
        )
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let cols =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(inner);

    // ── File list ──
    let items: Vec<ListItem> = b
        .entries
        .iter()
        .map(|it| {
            if it.is_dir {
                let label = if it.name == ".." {
                    "  ..".to_string()
                } else {
                    format!("📁 {}/", it.name)
                };
                ListItem::new(label).style(Style::default().fg(Color::Cyan))
            } else {
                ListItem::new(format!("🎵 {}", it.name)).style(Style::default().fg(Color::White))
            }
        })
        .collect();

    let mut list_state = ListState::default();
    if !b.entries.is_empty() {
        list_state.select(Some(b.cursor));
    }
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().fg(Color::Yellow).bold());
    frame.render_stateful_widget(list, cols[0], &mut list_state);

    // ── Preview pane ──
    let preview_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(" Preview ").fg(Color::Magenta));
    let preview_inner = preview_block.inner(cols[1]);
    frame.render_widget(preview_block, cols[1]);

    let lines: Vec<Line> = b
        .preview
        .iter()
        .map(|l| Line::from(l.as_str()).fg(Color::Gray))
        .collect();
    frame.render_widget(Paragraph::new(lines), preview_inner);
}

// ── Format helpers ────────────────────────────────────────────────────────

fn fmt_freq(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{:.1}kHz", hz / 1000.0)
    } else {
        format!("{:.0}Hz", hz)
    }
}
