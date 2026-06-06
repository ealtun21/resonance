use crate::{
    app::{App, BandField, EFFECT_NAMES, InputMode, Panel, fx_enabled, fx_intensity, fx_min},
    browser::Browser,
    curve,
    settings::{ConfirmAction, SettingsState, TABS},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Chart, Clear, Dataset, Gauge, GraphType, List, ListItem,
        ListState, Paragraph,
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
    if let InputMode::SelectOutput { sinks, cursor } = &app.mode {
        render_output_selector(sinks, *cursor, app, frame, frame.area());
    }
    if let InputMode::Settings(s) = &app.mode {
        render_settings(s, app, frame, frame.area());
    }
    if let InputMode::Help = &app.mode {
        render_help(frame, frame.area());
    }
}

// ── Help overlay ────────────────────────────────────────────────────────────

fn render_help(frame: &mut Frame, area: Rect) {
    let dialog = centered_rect(area, 60, 80);
    frame.render_widget(Clear, dialog);

    let key = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(
                format!("  {k:<14}"),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::styled(d.to_string(), Style::default().fg(Color::Gray)),
        ])
    };
    let head = |s: &str| {
        Line::from(Span::styled(
            format!(" {s}"),
            Style::default().fg(Color::Yellow).bold(),
        ))
    };

    let lines = vec![
        head("Navigation"),
        key("Tab", "switch panel (effects / bands)"),
        key("↑ ↓", "move selection"),
        key("Ctrl-z / Ctrl-y", "undo / redo"),
        key("? ", "toggle this help"),
        key("q / Ctrl-C", "quit"),
        Line::raw(""),
        head("Effects panel"),
        key("← →", "adjust intensity (Shift = ×2 step)"),
        key("Space", "toggle effect on/off"),
        Line::raw(""),
        head("Bands panel"),
        key("← →", "adjust selected field"),
        key("Space", "toggle band on/off"),
        key("a", "add band"),
        key("d / Del", "remove band"),
        key("t", "cycle band type"),
        Line::raw(""),
        head("Global"),
        key("+ / -", "preamp ±0.5 dB"),
        key("p", "power on/off"),
        key("l", "load preset (file browser)"),
        key("o", "select output device"),
        key("s", "settings (profiles / mappings)"),
        Line::raw(""),
        Line::from(Span::styled(
            "  press any key to close",
            Style::default().fg(Color::DarkGray).italic(),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(
            " Keybindings ",
            Style::default().fg(Color::Magenta).bold(),
        ));
    frame.render_widget(Paragraph::new(lines).block(block), dialog);
}

// ── Footer / contextual help ───────────────────────────────────────────────

fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let common = "[Tab] focus  [↑↓] select  [←→] adjust  [+/-] preamp  [Space] toggle  [l] load  [s] settings  [o] output  [p] power  [?] help  [q] quit";
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
        .and_then(|s| s.active_output.as_deref().map(|o| s.sink_label(o)))
        .map(|o| format!("🔊 {o}"))
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

    // Live meters.
    let meters = app.state.as_ref().map(|s| s.meters).unwrap_or_default();
    // Fixed-width dB readout so the value changes without shifting later spans.
    let db = |lin: f32| {
        if lin <= 1e-6 {
            "-inf".to_string()
        } else {
            format!("{:+.0}", 20.0 * lin.log10())
        }
    };
    let db = |lin: f32| format!("{:>4}", db(lin));
    let clip_active = app
        .clip_until
        .map(|t| std::time::Instant::now() < t)
        .unwrap_or(false);
    let level_color = if clip_active {
        Color::Red
    } else {
        Color::Green
    };
    let in_str = format!("I {} dB", db(meters.in_peak));
    let out_str = format!("O {} dB", db(meters.out_peak));
    let dsp_str = format!("DSP {:>3.0}%", meters.dsp_load * 100.0);
    let dsp_color = if meters.dsp_load > 0.8 {
        Color::Red
    } else if meters.dsp_load > 0.5 {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let sep = || Span::styled(" │ ", Style::default().fg(Color::DarkGray));

    let mut spans = vec![
        Span::styled(" ♪ Resonance ", Style::default().fg(Color::Magenta).bold()),
        power_span,
        sep(),
        Span::styled(format!("preset {preset}"), Style::default().fg(Color::Cyan)),
        sep(),
        Span::styled(sr, Style::default().fg(Color::DarkGray)),
        sep(),
        Span::styled(preamp, Style::default().fg(preamp_color)),
        sep(),
        Span::styled(in_str, Style::default().fg(level_color)),
        sep(),
        Span::styled(out_str, Style::default().fg(level_color)),
        sep(),
        Span::styled(dsp_str, Style::default().fg(dsp_color)),
        sep(),
    ];
    // Always reserve the clip slot so it flips OK→CLIP in place without shifting.
    if clip_active {
        spans.push(Span::styled("CLIP", Style::default().fg(Color::Red).bold()));
    } else {
        spans.push(Span::styled(" OK ", Style::default().fg(Color::DarkGray)));
    }
    if !output_str.is_empty() {
        spans.push(sep());
        spans.push(Span::styled(output_str, Style::default().fg(Color::Cyan)));
    }
    spans.push(Span::styled(status_str, Style::default().fg(status_color)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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

    let bins = Some(app.spectrum_display.as_slice());
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

    let full_names = crate::layout::band_type_full(inner.width);

    // Per-column alignment: numbers right, enable centred, type/bar left.
    let align = |c: usize| match c {
        0 | 2 | 3 | 4 => Alignment::Right,
        5 => Alignment::Center,
        _ => Alignment::Left,
    };
    // band_columns has a spacer rect at index 5; map logical column → rect index.
    let ri = |c: usize| if c >= 5 { c + 1 } else { c };

    // ── Header row ──
    let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let hcols = crate::layout::band_columns(header_rect);
    let headers: [&str; 7] = if full_names {
        ["#", "Type", "Freq", "Gain", "Q", "Enabled", "Gain Graph"]
    } else {
        ["#", "Type", "Hz", "dB", "Q", "On", ""]
    };
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
        // Type/Freq/Gain headers track their data alignment; the rest centre.
        let halign = match c {
            1 => Alignment::Left,          // Type
            0 | 2 | 3 => Alignment::Right, // #, Freq, Gain
            _ => Alignment::Center,        // Q, Enabled, Level
        };
        frame.render_widget(
            Paragraph::new(*h).style(style).alignment(halign),
            hcols[ri(c)],
        );
    }

    // ── Data rows (with scroll) ──
    let visible = (inner.height - 1) as usize;
    let offset = crate::layout::band_scroll_offset(app.band_cursor, bands.len(), visible);

    for (vis, i) in (offset..bands.len()).take(visible).enumerate() {
        let b = &bands[i];
        let y = inner.y + 1 + vis as u16;
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let cols = crate::layout::band_columns(row_rect);
        let selected = focused && app.band_cursor == i;

        // Subtle stripe + arrow on the selected row so it reads as a row, not a cell.
        if selected {
            frame.render_widget(
                Block::default().style(Style::default().bg(Color::Rgb(28, 28, 36))),
                row_rect,
            );
        }

        let type_name = if full_names {
            b.band_type.full().to_string()
        } else {
            b.band_type.abbrev().to_string()
        };
        let enable = if full_names {
            if b.enabled { "● On" } else { "○ Off" }
        } else if b.enabled {
            "●"
        } else {
            "○"
        };
        let bar = gain_bar(b.gain_db, cols[ri(6)].width as usize);
        let cells = [
            format!("{}", i + 1),
            type_name,
            fmt_freq(b.freq),
            format!("{:+.1}", b.gain_db),
            format!("{:.2}", b.q),
            enable.to_string(),
            bar,
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
                    1 => Color::Cyan,               // type
                    2 => freq_color(b.freq),        // freq across the spectrum
                    3 | 6 => gain_color(b.gain_db), // gain text + bar
                    5 => Color::Green,              // enabled dot
                    _ => Color::Gray,               // # and Q
                };
                let s = Style::default().fg(fg);
                if selected { s.bold() } else { s }
            };
            frame.render_widget(
                Paragraph::new(text.as_str())
                    .style(style)
                    .alignment(align(c)),
                cols[ri(c)],
            );
        }
    }
}

/// Horizontal gain bar: fills outward from a centre tick — right for boosts,
/// left for cuts — scaled to ±`DB_RANGE`. Returns a `width`-char string.
fn gain_bar(db: f64, width: usize) -> String {
    if width < 3 {
        return String::new();
    }
    let centre = width / 2;
    let half = centre.min(width - centre - 1);
    let t = (db / DB_RANGE).clamp(-1.0, 1.0);
    let n = (t.abs() * half as f64).round() as usize;
    let mut cells = vec![' '; width];
    cells[centre] = if n == 0 { '┊' } else { '│' };
    if db >= 0.0 {
        for k in 1..=n {
            cells[centre + k] = '█';
        }
    } else {
        for k in 1..=n {
            cells[centre - k] = '█';
        }
    }
    cells.into_iter().collect()
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

// ── Output selector popup ─────────────────────────────────────────────────

fn render_output_selector(
    sinks: &[String],
    cursor: usize,
    app: &App,
    frame: &mut Frame,
    area: Rect,
) {
    let dialog = centered_rect(area, 60, 50);
    let min_h = (sinks.len() as u16 + 4).max(6);
    let dialog = Rect::new(dialog.x, dialog.y, dialog.width, dialog.height.max(min_h));
    frame.render_widget(Clear, dialog);

    let active = app
        .state
        .as_ref()
        .and_then(|s| s.preferred_output.as_deref().or(s.active_output.as_deref()));

    let block = Block::default()
        .title(Line::from(" Select Output Sink ").fg(Color::Yellow))
        .title_bottom(Line::from(" ↑↓ move   Enter select   Esc cancel ").fg(Color::DarkGray))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    if sinks.is_empty() {
        frame.render_widget(
            Paragraph::new(" (no sinks detected)").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = sinks
        .iter()
        .map(|s| {
            let is_active = active.map(|a| a == s).unwrap_or(false);
            let name = app
                .state
                .as_ref()
                .map(|st| st.sink_label(s))
                .unwrap_or_else(|| s.clone());
            let label = if is_active {
                format!("● {name}")
            } else {
                format!("  {name}")
            };
            let style = if is_active {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(label).style(style)
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(cursor));
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().fg(Color::Yellow).bold());
    frame.render_stateful_widget(list, inner, &mut list_state);
}

// ── Settings popup ─────────────────────────────────────────────────────────

fn render_settings(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let dialog = centered_rect(area, 70, 80);
    frame.render_widget(Clear, dialog);

    let hint = settings_footer_hint(s);
    let block = Block::default()
        .title(Line::from(" Settings ").fg(Color::Yellow))
        .title_bottom(Line::from(hint).fg(Color::DarkGray))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let cols = Layout::horizontal([Constraint::Length(16), Constraint::Min(1)]).split(inner);

    let tab_col = cols[0];
    let content_col = cols[1];

    // Right-border divider on the tab column.
    let tab_border = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(tab_border, tab_col);

    // Tab list (leave 1 col for the right border).
    let tab_inner = Rect::new(
        tab_col.x,
        tab_col.y,
        tab_col.width.saturating_sub(1),
        tab_col.height,
    );
    render_settings_tabs(s, frame, tab_inner);

    // Content area (1 col padding on left).
    let content_inner = Rect::new(
        content_col.x + 1,
        content_col.y,
        content_col.width.saturating_sub(1),
        content_col.height,
    );
    render_settings_content(s, app, frame, content_inner);

    // Overlays (drawn on top of dialog, so they use dialog coords).
    if s.confirm.is_some() {
        render_confirm(s, frame, dialog);
    } else if s.sub_picker.is_some() {
        render_sub_picker(s, frame, dialog);
    }
}

fn settings_footer_hint(s: &SettingsState) -> String {
    let base = " [Tab/←→/1-5] switch  [↑↓] select  [Esc] close";
    let ctx = match s.tab {
        0 => "  •  [Enter] load  [n] save  [r] rename  [d] delete",
        1 => "  •  [m] map  [d] unmap",
        2 => "  •  [Enter] route  [m] map to profile",
        3 => "  •  [Enter/Space] edit/toggle",
        4 => "  •  [Enter] run action",
        _ => "",
    };
    format!("{base}{ctx}")
}

fn render_settings_tabs(s: &SettingsState, frame: &mut Frame, area: Rect) {
    for (i, name) in TABS.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let row = Rect::new(area.x, y, area.width, 1);
        let active = s.tab == i;
        let label = format!("[{}] {}", i + 1, name);
        let (marker, style) = if active {
            ("▶", Style::default().fg(Color::Cyan).bold())
        } else {
            (" ", Style::default().fg(Color::DarkGray))
        };
        frame.render_widget(
            Paragraph::new(format!("{marker} {label}")).style(style),
            row,
        );
    }
}

fn render_settings_content(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    match s.tab {
        0 => render_tab_profiles(s, app, frame, area),
        1 => render_tab_mappings(s, app, frame, area),
        2 => render_tab_devices(s, app, frame, area),
        3 => render_tab_prefs(s, app, frame, area),
        4 => render_tab_daemon(s, app, frame, area),
        _ => {}
    }
}

fn render_tab_profiles(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let content = rows[0];
    let hints = rows[1];

    // "(active)" = the profile currently loaded onto the chain. LoadProfile sets
    // current_preset to the profile name, so match on that (mapped_profile only
    // tracks output→profile auto-loads, which is a different concept).
    let active_profile = app
        .state
        .as_ref()
        .and_then(|st| st.current_preset.as_deref());

    if s.profiles.is_empty() {
        frame.render_widget(
            Paragraph::new("(no profiles saved — press 'n' to save current chain)")
                .style(Style::default().fg(Color::DarkGray)),
            content,
        );
    } else {
        let visible = content.height as usize;
        let offset = crate::layout::band_scroll_offset(s.cursor, s.profiles.len(), visible);
        for (vis, i) in (offset..s.profiles.len()).take(visible).enumerate() {
            let name = &s.profiles[i];
            let y = content.y + vis as u16;
            let row = Rect::new(content.x, y, content.width, 1);
            let is_active = active_profile == Some(name.as_str());
            let selected = i == s.cursor;
            let marker = if selected { "▶" } else { " " };
            let suffix = if is_active { "  (active)" } else { "" };
            let style = if selected {
                Style::default().fg(Color::Yellow).bold()
            } else if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            frame.render_widget(
                Paragraph::new(format!("{marker} {name}{suffix}")).style(style),
                row,
            );
        }
    }

    frame.render_widget(
        Paragraph::new(" [Enter] load  [n] save current  [r] rename  [d] delete")
            .style(Style::default().fg(Color::DarkGray)),
        hints,
    );

    // Inline text input overlay (save profile name).
    if let Some(ti) = &s.text_input {
        render_text_input_overlay(ti.label, &ti.buf, ti.cursor, frame, area);
    }
}

fn render_tab_mappings(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let status_row = rows[0];
    let header_row = rows[1];
    let list_area = rows[2];
    let hints = rows[3];

    let active_output = app
        .state
        .as_ref()
        .and_then(|st| st.active_output.as_deref());
    let mapped_profile = app
        .state
        .as_ref()
        .and_then(|st| st.mapped_profile.as_deref());

    let active_label = active_output
        .map(|o| {
            app.state
                .as_ref()
                .map(|st| st.sink_label(o))
                .unwrap_or_else(|| o.to_string())
        })
        .unwrap_or_else(|| "none".to_string());
    frame.render_widget(
        Paragraph::new(format!(
            "Active: {}   Mapped profile: {}",
            active_label,
            mapped_profile.unwrap_or("none")
        ))
        .style(Style::default().fg(Color::Cyan)),
        status_row,
    );

    frame.render_widget(
        Paragraph::new(" Output Device                    Profile")
            .style(Style::default().fg(Color::DarkGray).bold()),
        header_row,
    );

    if s.mappings.is_empty() {
        frame.render_widget(
            Paragraph::new(" (no mappings — select a device and press 'm')")
                .style(Style::default().fg(Color::DarkGray)),
            list_area,
        );
    } else {
        let visible = list_area.height as usize;
        let offset = crate::layout::band_scroll_offset(s.cursor, s.mappings.len(), visible);
        for (vis, i) in (offset..s.mappings.len()).take(visible).enumerate() {
            let (out, profile) = &s.mappings[i];
            let y = list_area.y + vis as u16;
            let row = Rect::new(list_area.x, y, list_area.width, 1);
            let is_active = active_output == Some(out.as_str());
            let selected = i == s.cursor;
            let marker = if is_active { "●" } else { " " };
            let style = if selected {
                Style::default().fg(Color::Yellow).bold()
            } else if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            let out = app
                .state
                .as_ref()
                .map(|st| st.sink_label(out))
                .unwrap_or_else(|| out.clone());
            frame.render_widget(
                Paragraph::new(format!(" {marker} {out:<32} {profile}")).style(style),
                row,
            );
        }
    }

    frame.render_widget(
        Paragraph::new(" [m] map current output to profile  [d] unmap selected")
            .style(Style::default().fg(Color::DarkGray)),
        hints,
    );
}

fn render_tab_devices(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let content = rows[0];
    let hints = rows[1];

    let active_output = app
        .state
        .as_ref()
        .and_then(|st| st.active_output.as_deref());

    if s.sinks.is_empty() {
        frame.render_widget(
            Paragraph::new("(no audio sinks detected)").style(Style::default().fg(Color::DarkGray)),
            content,
        );
    } else {
        let visible = content.height as usize;
        let offset = crate::layout::band_scroll_offset(s.cursor, s.sinks.len(), visible);
        for (vis, i) in (offset..s.sinks.len()).take(visible).enumerate() {
            let sink = &s.sinks[i];
            let y = content.y + vis as u16;
            let row = Rect::new(content.x, y, content.width, 1);
            let is_active = active_output == Some(sink.as_str());
            let selected = i == s.cursor;
            let has_mapping = s.mappings.iter().any(|(out, _)| out == sink);
            let active_mark = if is_active { "●" } else { " " };
            let map_mark = if has_mapping { "  [mapped]" } else { "" };
            let style = if selected {
                Style::default().fg(Color::Yellow).bold()
            } else if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            let sink = app
                .state
                .as_ref()
                .map(|st| st.sink_label(sink))
                .unwrap_or_else(|| sink.clone());
            frame.render_widget(
                Paragraph::new(format!(" {active_mark} {sink}{map_mark}")).style(style),
                row,
            );
        }
    }

    frame.render_widget(
        Paragraph::new(" [Enter] route output here  [m] map to auto-load profile")
            .style(Style::default().fg(Color::DarkGray)),
        hints,
    );
}

fn render_tab_prefs(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let prefs = &app.prefs;
    let items: [(&str, String, &str); 5] = [
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
    ];

    for (i, (label, value, desc)) in items.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let row = Rect::new(area.x, y, area.width, 1);
        let selected = s.cursor == i;
        let editing = selected && s.text_input.is_some();

        let value_display = if editing {
            if let Some(ti) = &s.text_input {
                let mut buf = ti.buf.clone();
                buf.insert(ti.cursor, '█');
                buf
            } else {
                value.clone()
            }
        } else {
            value.clone()
        };

        let marker = if selected { "▶" } else { " " };
        let label_style = if selected {
            Style::default().fg(Color::Yellow).bold()
        } else {
            Style::default().fg(Color::White)
        };
        let val_style = if editing {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::Cyan)
        };
        let desc_style = Style::default().fg(Color::DarkGray);

        let line = Line::from(vec![
            Span::styled(format!("{marker} {label:<18}  "), label_style),
            Span::styled(value_display, val_style),
            Span::styled(format!("  {desc}"), desc_style),
        ]);
        frame.render_widget(Paragraph::new(line), row);
    }
}

fn render_tab_daemon(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let st = app.daemon_status;
    // Status summary line.
    let on = |b: bool, yes: &str, no: &str| {
        if b {
            Span::styled(yes.to_string(), Style::default().fg(Color::Green).bold())
        } else {
            Span::styled(no.to_string(), Style::default().fg(Color::DarkGray))
        }
    };
    let summary = Line::from(vec![
        Span::styled("status  ", Style::default().fg(Color::White)),
        on(st.active, "● running", "○ stopped"),
        Span::styled("   ", Style::default()),
        on(st.enabled, "autostart on", "autostart off"),
    ]);
    let header = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(Paragraph::new(summary), header);

    let autostart_label = if st.enabled {
        "Autostart at login  [on] "
    } else {
        "Autostart at login  [off]"
    };
    let items: [(&str, &str); 4] = [
        ("Start", "launch the daemon now (installs the service)"),
        ("Stop", "stop the running daemon"),
        ("Restart", "restart the daemon"),
        (autostart_label, "toggle start at login"),
    ];

    for (i, (label, desc)) in items.iter().enumerate() {
        let y = area.y + 2 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let row = Rect::new(area.x, y, area.width, 1);
        let selected = s.cursor == i;
        let marker = if selected { "▶" } else { " " };
        let label_style = if selected {
            Style::default().fg(Color::Yellow).bold()
        } else {
            Style::default().fg(Color::White)
        };
        let line = Line::from(vec![
            Span::styled(format!("{marker} {label:<26}"), label_style),
            Span::styled(format!("  {desc}"), Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), row);
    }
}

fn render_text_input_overlay(
    label: &str,
    buf: &str,
    cursor: usize,
    frame: &mut Frame,
    parent: Rect,
) {
    let mut display = buf.to_string();
    display.insert(cursor, '█');

    let w =
        (label.len() as u16 + display.len() as u16 + 8).clamp(30, parent.width.saturating_sub(4));
    let h = 3;
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    let dialog = Rect::new(x, y, w, h);

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .title(Line::from(format!(" {label} ")).fg(Color::Yellow))
        .title_bottom(Line::from(" Enter confirm  Esc cancel ").fg(Color::DarkGray))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);
    frame.render_widget(
        Paragraph::new(display).style(Style::default().fg(Color::White)),
        inner,
    );
}

fn render_confirm(s: &SettingsState, frame: &mut Frame, parent: Rect) {
    let msg = match &s.confirm {
        Some(ConfirmAction::DeleteProfile(name)) => format!(" Delete profile '{name}'? [y/n] "),
        Some(ConfirmAction::UnmapOutput) => " Remove this output mapping? [y/n] ".to_string(),
        None => return,
    };
    let w = (msg.len() as u16 + 4)
        .min(parent.width.saturating_sub(4))
        .max(30);
    let h = 3u16;
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    let dialog = Rect::new(x, y, w, h);

    frame.render_widget(Clear, dialog);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);
    frame.render_widget(
        Paragraph::new(msg).style(Style::default().fg(Color::White)),
        inner,
    );
}

fn render_sub_picker(s: &SettingsState, frame: &mut Frame, parent: Rect) {
    let Some(sp) = &s.sub_picker else { return };
    let w = (parent.width * 60 / 100).max(30);
    let h = ((sp.profiles.len() as u16).saturating_add(4))
        .min(parent.height * 70 / 100)
        .max(6);
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    let dialog = Rect::new(x, y, w, h);

    frame.render_widget(Clear, dialog);

    let title = if sp.for_sink.is_some() {
        " Map Device → Profile "
    } else {
        " Map Output → Profile "
    };
    let block = Block::default()
        .title(Line::from(title).fg(Color::Yellow))
        .title_bottom(Line::from(" ↑↓ move   Enter select   Esc cancel ").fg(Color::DarkGray))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let items: Vec<ListItem> = sp
        .profiles
        .iter()
        .map(|p| ListItem::new(format!("  {p}")).style(Style::default().fg(Color::White)))
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(sp.cursor));
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().fg(Color::Yellow).bold());
    frame.render_stateful_widget(list, inner, &mut list_state);
}

// ── Format helpers ────────────────────────────────────────────────────────

fn fmt_freq(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{:.1}kHz", hz / 1000.0)
    } else {
        format!("{:.0}Hz", hz)
    }
}
