use crate::{
    app::{
        App, BandField, EFFECT_NAMES, InputMode, Panel, SquigTab, fx_enabled, fx_intensity, fx_min,
    },
    browser::Browser,
    curve,
    settings::{ConfirmAction, SettingsState, TABS},
};
use resonance_ipc::{AppStream, ChannelMask, RoutingMatrix, SinkVolume};
use resonance_reference::reference::SeriesRole;

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
    let p = crate::layout::panes(
        frame.area(),
        app.prefs.show_spectrum,
        app.apps_visible(),
        app.sinks_visible(),
    );

    render_status(app, frame, p.status);
    render_eq_curve(app, frame, p.eq);
    if app.prefs.show_spectrum {
        render_spectrum(app, frame, p.spectrum);
    }
    render_effects(app, frame, p.effects);
    render_bands(app, frame, p.bands);
    if app.apps_visible() {
        render_apps(app, frame, p.apps);
    }
    if app.sinks_visible() {
        render_sinks(app, frame, p.sinks);
    }
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
    if let InputMode::SelectBandChannels {
        index,
        mask,
        cursor,
    } = &app.mode
    {
        render_band_channels(*index, *mask, *cursor, app, frame, frame.area());
    }
    if let InputMode::SquigBrowse { tab, query, cursor } = &app.mode {
        render_squig_browse(*tab, query, *cursor, app, frame, frame.area());
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
        key("Tab", "switch panel (effects / bands / graph)"),
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
        key("S", "cycle band slope 12/24/48 dB/oct (shelf, HP/LP)"),
        key("M", "cycle band scope stereo/mid/side (≥2ch)"),
        key("c", "channel targeting (multichannel)"),
        Line::raw(""),
        head("FR graph (Tab to it, or use the mouse)"),
        key("↑↓ / ←→", "drag node: gain / frequency"),
        key("[ ]", "select prev / next node"),
        key("mouse drag", "move node (left) · tune Q (right)"),
        key("mouse wheel", "nudge node gain"),
        Line::raw(""),
        head("Global"),
        key("+ / -", "preamp ±0.5 dB"),
        key("D", "cycle output dither (off / 16 / 20 / 24-bit)"),
        key("p", "power on/off"),
        key("w", "swap L/R channels (≥2ch)"),
        key("l", "load preset (file browser)"),
        key("o", "select output device"),
        key("A", "show/hide Applications panel"),
        key("O", "show/hide Outputs panel"),
        key("s", "settings — profiles, devices, reference + Auto-EQ"),
        key("s → 6", "Reference tab: target curve, measurement, Auto-EQ"),
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
    let line = Line::from(vec![
        Span::styled(format!(" {common}"), Style::default().fg(Color::DarkGray)),
        Span::styled(ctx, Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

// ── Status bar ────────────────────────────────────────────────────────────

/// Sample-rate readout: a single rate when capture and DSP rates match, or
/// `capture→DSP` when a resampler is engaged (rates differ by >1 Hz).
fn sample_rate_label(capture_rate: f64, sample_rate: f64) -> String {
    if (capture_rate - sample_rate).abs() > 1.0 {
        format!("{capture_rate:.0}→{sample_rate:.0} Hz")
    } else {
        format!("{sample_rate:.0} Hz")
    }
}

/// Preamp readout and colour: "preamp 0 dB" (dim) within the ±0.05 dB dead
/// zone, otherwise the signed value in cyan.
fn preamp_label(preamp_db: f64) -> (String, Color) {
    if preamp_db.abs() < 0.05 {
        ("preamp 0 dB".to_string(), Color::DarkGray)
    } else {
        (format!("preamp {preamp_db:+.1} dB"), Color::Cyan)
    }
}

/// Fixed-width (4-char) dBFS readout for a linear peak level: `-inf` below the
/// noise floor, otherwise the signed dB value. Right-padded so the value
/// changes in place without shifting the spans after it.
fn meter_db(lin: f32) -> String {
    let v = if lin <= 1e-6 {
        "-inf".to_string()
    } else {
        format!("{:+.0}", 20.0 * lin.log10())
    };
    format!("{v:>4}")
}

/// DSP-load colour: red when over 80% (risking dropouts), yellow over 50%, dim
/// otherwise.
fn dsp_load_color(load: f32) -> Color {
    if load > 0.8 {
        Color::Red
    } else if load > 0.5 {
        Color::Yellow
    } else {
        Color::DarkGray
    }
}

/// Channel / routing summary, surfaced only when the path isn't plain stereo
/// passthrough (progressive disclosure — stereo users see nothing here).
///
/// Returns e.g. `6ch`, `2→6ch` (up/down-mix), with a `L⇄R` tag for a pure swap
/// or `routed` for any other routing matrix. `None` when it's uninteresting
/// stereo passthrough.
fn channel_summary(
    channels: usize,
    out_channels: usize,
    routing: Option<&RoutingMatrix>,
) -> Option<String> {
    let routed = routing.is_some();
    let swapped = channels >= 2 && routing == Some(&RoutingMatrix::swap(channels, 0, 1));
    let interesting = channels != 2 || out_channels != channels || routed;
    if !interesting {
        return None;
    }
    let mut t = if out_channels != 0 && out_channels != channels {
        format!("{channels}→{out_channels}ch")
    } else {
        format!("{channels}ch")
    };
    if swapped {
        t.push_str(" L⇄R");
    } else if routed {
        t.push_str(" routed");
    }
    Some(t)
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let power_span = if app.state.as_ref().is_some_and(|s| s.enabled) {
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
        .map(|s| sample_rate_label(s.capture_rate, s.sample_rate))
        .unwrap_or_default();

    let preamp_db = app.state.as_ref().map_or(0.0, |s| s.preamp_db);
    let (preamp, preamp_color) = preamp_label(preamp_db);

    let dither_bits = app.state.as_ref().and_then(|s| s.dither_bits);
    let (dither, dither_color) = match dither_bits {
        None => ("dither off".to_string(), Color::DarkGray),
        Some(b) => (format!("dither {b}-bit"), Color::Cyan),
    };

    let output_str = app
        .state
        .as_ref()
        .and_then(|s| s.active_output.as_deref().map(|o| s.sink_label(o)))
        .map(|o| format!("🔊 {o}"))
        .unwrap_or_default();

    let status = app.status_text();
    let status_color = if status.is_empty() {
        Color::DarkGray
    } else {
        Color::Yellow
    };
    let status_str = if status.is_empty() {
        String::new()
    } else {
        format!("  {status}")
    };

    // Live meters.
    let meters = app.state.as_ref().map(|s| s.meters).unwrap_or_default();
    let clip_active = app
        .clip_until
        .is_some_and(|t| std::time::Instant::now() < t);
    let level_color = if clip_active {
        Color::Red
    } else {
        Color::Green
    };
    let in_str = format!("I {} dB", meter_db(meters.in_peak));
    let out_str = format!("O {} dB", meter_db(meters.out_peak));
    let dsp_str = format!("DSP {:>3.0}%", meters.dsp_load * 100.0);
    let dsp_color = dsp_load_color(meters.dsp_load);

    let ch_str = app
        .state
        .as_ref()
        .and_then(|s| channel_summary(s.channels, s.out_channels, s.routing.as_ref()));

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
        Span::styled(dither, Style::default().fg(dither_color)),
        sep(),
    ];
    if let Some(ch) = ch_str {
        spans.push(Span::styled(ch, Style::default().fg(Color::Cyan)));
        spans.push(sep());
    }
    spans.push(Span::styled(in_str, Style::default().fg(level_color)));
    spans.push(sep());
    spans.push(Span::styled(out_str, Style::default().fg(level_color)));
    spans.push(sep());
    spans.push(Span::styled(dsp_str, Style::default().fg(dsp_color)));
    spans.push(sep());
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

/// One reference-overlay polyline ready to plot: (colour, legend name, points).
type RefRun = (Color, &'static str, Vec<(f64, f64)>);

/// Build the EQ-graph title: panel name, a "· reference" tag in reference mode,
/// and — in Graph-edit focus — a live readout of the selected node so the user
/// sees what they are dragging.
fn eq_curve_title(app: &App, graph_focus: bool) -> Vec<Span<'static>> {
    let mut title = vec![Span::styled(
        " EQ Frequency Response ",
        Style::default().fg(Color::Magenta),
    )];
    if app.reference.active() {
        title.push(Span::styled(
            "· reference ",
            Style::default().fg(Color::DarkGray),
        ));
    }
    if graph_focus {
        if let Some(b) = app
            .state
            .as_ref()
            .and_then(|s| s.bands.get(app.band_cursor))
        {
            title.push(Span::styled(
                format!(
                    "· edit {} {:+.1}dB Q{:.2} ",
                    fmt_freq(b.freq),
                    b.gain_db,
                    b.q
                ),
                Style::default().fg(Color::Yellow).bold(),
            ));
        }
    }
    title
}

/// Map a band to its graph marker position `(log10(freq), clamped gain_db)`.
/// Clamping keeps an out-of-range gain pinned to the chart edge instead of
/// jumping off it.
fn band_marker_point(b: &resonance_ipc::BandState) -> (f64, f64) {
    (
        curve::band_marker_x(b.freq),
        b.gain_db.clamp(-DB_RANGE, DB_RANGE),
    )
}

/// A set of `(log-freq-x, gain-db-y)` plot points.
type MarkerPts = Vec<(f64, f64)>;

/// Split the enabled bands' markers into (all-but-selected, selected) so the
/// selected node can be drawn last (on top) in its own highlight colour.
fn band_markers(bands: &[resonance_ipc::BandState], sel: usize) -> (MarkerPts, MarkerPts) {
    let others = bands
        .iter()
        .enumerate()
        .filter(|(i, b)| b.enabled && *i != sel)
        .map(|(_, b)| band_marker_point(b))
        .collect();
    let selected = bands
        .get(sel)
        .filter(|b| b.enabled)
        .map(band_marker_point)
        .into_iter()
        .collect();
    (others, selected)
}

/// Top-to-bottom vertical guide line through the selected band's frequency
/// (Graph-edit mode only), so the column being dragged is obvious. Empty when
/// not in Graph focus or the cursor is out of range.
fn selected_guide_points(
    bands: &[resonance_ipc::BandState],
    cursor: usize,
    graph_focus: bool,
) -> Vec<(f64, f64)> {
    if !graph_focus {
        return Vec::new();
    }
    bands
        .get(cursor)
        .map(|b| {
            let x = curve::band_marker_x(b.freq);
            vec![(x, -DB_RANGE), (x, DB_RANGE)]
        })
        .unwrap_or_default()
}

/// Map reference series into plottable runs: a fixed colour + legend name per
/// role, with each series' dB clamped to the chart's ±range. Result is white
/// (the curve shaped onto the target) — yellow is reserved for the selected
/// band marker, so it is deliberately not used here.
fn reference_runs(series: &[resonance_reference::reference::RefSeries]) -> Vec<RefRun> {
    series
        .iter()
        .map(|s| {
            let (color, name) = match s.role {
                SeriesRole::Target => (Color::Magenta, "Target"),
                SeriesRole::Result => (Color::White, "Result"),
                SeriesRole::Measurement => (Color::DarkGray, "Meas"),
            };
            let pts = s
                .pts
                .iter()
                .map(|&(x, y)| (x, y.clamp(-DB_RANGE, DB_RANGE)))
                .collect();
            (color, name, pts)
        })
        .collect()
}

/// Fill points for the listener-preference tolerance band around the target
/// (asymmetric below/above). Returns a dim half-block scatter that reads as a
/// solid shaded region. Columns are subsampled to ~`width` and the vertical
/// fill steps ~0.5 dB so the point count stays bounded. Empty unless a Target
/// series is present.
fn preference_shade_points(
    series: &[resonance_reference::reference::RefSeries],
    width: u16,
) -> Vec<(f64, f64)> {
    let Some(target) = series.iter().find(|s| s.role == SeriesRole::Target) else {
        return Vec::new();
    };
    let step = (target.pts.len() / (width.max(1) as usize)).max(1);
    let mut pts = Vec::new();
    for (i, &(lf, ty)) in target.pts.iter().enumerate() {
        if i % step != 0 {
            continue;
        }
        let (below, above) = resonance_reference::reference::preference_bounds(10f64.powf(lf));
        let lo = (ty - below).clamp(-DB_RANGE, DB_RANGE);
        let hi = (ty + above).clamp(-DB_RANGE, DB_RANGE);
        let mut y = lo;
        while y <= hi {
            pts.push((lf, y));
            y += 0.5;
        }
    }
    pts
}

/// The ±`DB_RANGE` y-axis tick labels (-N / 0 / +N), with the zero tick brighter.
fn eq_y_axis_labels() -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("-{DB_RANGE:.0}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" 0", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("+{DB_RANGE:.0}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

/// A single-coloured polyline dataset (used for the zero line, guide, and
/// coloured response runs).
fn line_dataset(color: Color, pts: &[(f64, f64)]) -> Dataset<'_> {
    Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(color))
        .data(pts)
}

fn render_eq_curve(app: &App, frame: &mut Frame, area: Rect) {
    // The graph reads as "focused" (cyan border, highlighted node) for both the
    // Bands table and the interactive Graph panel.
    let graph_focus = app.focus == Panel::Graph;
    let focused = app.focus == Panel::Bands || graph_focus;

    let block = Block::default()
        .title(Line::from(eq_curve_title(app, graph_focus)))
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

    let ticks = curve::x_axis_ticks();
    // x_axis_ticks() is a non-empty constant list, so first/last always exist.
    let log_min = ticks[0].0;
    let log_max = ticks[ticks.len() - 1].0;

    // Band markers: a dot at (log10(freq), gain) for each enabled band, with the
    // selected one split out so it draws last in its highlight colour.
    let sel = if focused { app.band_cursor } else { usize::MAX };
    let (markers_other, marker_sel) = band_markers(&bands, sel);
    let guide_pts = selected_guide_points(&bands, app.band_cursor, graph_focus);

    let zero_pts: Vec<(f64, f64)> = vec![(log_min, 0.0), (log_max, 0.0)];

    let x_labels: Vec<Span> = ticks
        .iter()
        .map(|(_, label)| Span::styled(*label, Style::default().fg(Color::DarkGray)))
        .collect();
    let y_labels = eq_y_axis_labels();

    // Reference overlay (target / measurement / result) and its tolerance band,
    // when active — all in the same (log10 freq, dB) space as the EQ curve.
    let ref_series = app.reference.series(
        &bands,
        sr,
        n_points,
        log_min,
        log_max,
        if app.reference.normalized { 1.0 } else { 0.0 },
    );
    let ref_runs = reference_runs(&ref_series);
    let shade_pts = if app.reference.show_bounds {
        preference_shade_points(&ref_series, inner.width)
    } else {
        Vec::new()
    };

    // Colour-code the response by gain sign: boost green, cut red, neutral cyan
    // near 0 dB (matches the GUI's gain-signed curve tint). Order: shaded band
    // (backdrop), zero reference, coloured curve runs, reference overlay, then
    // the band markers (on top).
    //
    // In reference mode the plain EQ-response runs are HIDDEN — the "Result"
    // series (measurement shaped by the current EQ) carries the same
    // information, and drawing both made the graph a tangle of lines (matches
    // the GUI, which swaps the response for the result in reference mode).
    let reference_active = app.reference.active();
    let runs = curve_runs(&curve_data);
    let mut datasets = Vec::new();
    if !shade_pts.is_empty() {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::HalfBlock)
                .graph_type(GraphType::Scatter)
                .style(Style::default().fg(Color::Rgb(48, 48, 66)))
                .data(&shade_pts),
        );
    }
    datasets.push(line_dataset(Color::DarkGray, &zero_pts));
    if !reference_active {
        for (color, pts) in &runs {
            datasets.push(line_dataset(*color, pts));
        }
    }
    for (color, name, pts) in &ref_runs {
        datasets.push(line_dataset(*color, pts).name(*name));
    }
    // Selected-node vertical guide (Graph-edit mode), under the markers.
    if !guide_pts.is_empty() {
        datasets.push(line_dataset(Color::Rgb(90, 80, 40), &guide_pts));
    }
    datasets.push(
        Dataset::default()
            .marker(symbols::Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(Color::DarkGray))
            .data(&markers_other),
    );
    datasets.push(
        Dataset::default()
            .marker(symbols::Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(Color::Yellow).bold())
            .data(&marker_sel),
    );

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
        )
        // Legend (top-right) for the named reference series — only the named
        // datasets appear, so it's empty (hidden) when reference mode is off.
        .legend_position(Some(ratatui::widgets::LegendPosition::TopRight))
        .hidden_legend_constraints((Constraint::Ratio(1, 3), Constraint::Ratio(1, 3)));

    frame.render_widget(chart, inner);
}

/// Split the response polyline into contiguous colour runs by gain sign so the
/// curve renders boost-green / cut-red / neutral-cyan. Each run repeats the
/// previous run's last point so the coloured segments join seamlessly.
fn curve_runs(points: &[(f64, f64)]) -> Vec<(Color, Vec<(f64, f64)>)> {
    fn bucket(g: f64) -> u8 {
        if g >= 1.0 { 2 } else { u8::from(g > -1.0) }
    }
    fn colour(bk: u8) -> Color {
        match bk {
            2 => Color::Green,
            0 => Color::LightRed,
            _ => Color::Cyan,
        }
    }
    let mut runs: Vec<(Color, Vec<(f64, f64)>)> = Vec::new();
    let mut cur: Option<u8> = None;
    for &p in points {
        let bk = bucket(p.1);
        if cur == Some(bk) {
            runs.last_mut().unwrap().1.push(p);
        } else {
            // Bridge to the previous run's endpoint so the line stays continuous
            // across the colour change.
            let mut seg = Vec::new();
            if let Some(prev) = runs.last().and_then(|r| r.1.last().copied()) {
                seg.push(prev);
            }
            seg.push(p);
            runs.push((colour(bk), seg));
            cur = Some(bk);
        }
    }
    runs
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
    // 8-level partial blocks (fill from bottom of a cell — used for upward bars).
    const LOWER: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
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
    let half = f64::from(rows);
    let center_up = inner.y + inner.height / 2; // first row of the lower half
    let n = bins.len();
    let buf = frame.buffer_mut();

    // Linearly interpolate the bins across the full width → smooth silhouette
    // where every column gets its own height (no flat, blocky plateaus).
    let w = inner.width;
    let sample = |data: &[f32], cx: u16| -> f64 {
        if w <= 1 {
            return f64::from(data[0].clamp(0.0, 1.0));
        }
        let pos = f64::from(cx) / f64::from(w - 1) * (n - 1) as f64;
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(n - 1);
        let frac = pos - i0 as f64;
        let a = f64::from(data[i0].clamp(0.0, 1.0));
        let b = f64::from(data[i1].clamp(0.0, 1.0));
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
            let color = spectrum_color(f64::from(r) / half);
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
                spectrum_color(f64::from(full) / half)
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
        .map_or((0.0, false), |s| (fx_intensity(s, idx), fx_enabled(s, idx)));

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

// ── Applications panel ──────────────────────────────────────────────────────

/// Per-application volume/mute strip: one row per app (mute glyph · name ·
/// volume gauge · percentage), mirroring the Effects rows. Shown only when the
/// daemon reports application streams.
fn render_apps(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Apps;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let count = app.state.as_ref().map_or(0, |s| s.apps.len());
    let block = Block::default()
        .title(
            Line::from(format!(" Applications · {count} ")).fg(if focused {
                Color::Cyan
            } else {
                Color::Magenta
            }),
        )
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(state) = app.state.as_ref() else {
        return;
    };
    let fit = inner.height as usize;
    let shown = state.apps.len().min(fit);
    if shown == 0 {
        return;
    }
    let rows = Layout::vertical(vec![Constraint::Length(1); shown]).split(inner);
    for (idx, line) in rows.iter().enumerate() {
        render_app_row(app, frame, &state.apps[idx], idx, *line, focused);
    }
}

/// The Outputs (per-output-sink) volume panel — the device-volume mirror of the
/// Applications panel. Same row layout (mute dot · name · gauge · %), keyed by
/// the sink's human description. Focus/select via Tab + arrows; `Space` mutes.
fn render_sinks(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Sinks;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let count = app.state.as_ref().map_or(0, |s| s.sinks.len());
    let block = Block::default()
        .title(Line::from(format!(" Outputs · {count} ")).fg(if focused {
            Color::Cyan
        } else {
            Color::Magenta
        }))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(state) = app.state.as_ref() else {
        return;
    };
    let fit = inner.height as usize;
    let shown = state.sinks.len().min(fit);
    if shown == 0 {
        return;
    }
    let rows = Layout::vertical(vec![Constraint::Length(1); shown]).split(inner);
    for (idx, line) in rows.iter().enumerate() {
        render_sink_row(app, frame, &state.sinks[idx], idx, *line, focused);
    }
}

fn render_sink_row(
    app: &App,
    frame: &mut Frame,
    sink: &SinkVolume,
    idx: usize,
    area: Rect,
    panel_focused: bool,
) {
    let selected = panel_focused && app.sink_cursor == idx;
    let label = if sink.description.is_empty() {
        &sink.name
    } else {
        &sink.description
    };
    let name_style = if selected {
        Style::default().fg(Color::Yellow).bold()
    } else if sink.muted {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let mute_sym = if sink.muted {
        Span::styled("○", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled("●", Style::default().fg(Color::Green))
    };
    let pct = (sink.volume * 100.0).round() as i32;
    let ratio = sink.volume.clamp(0.0, 1.0);

    // Layout mirrors the app row: mute(1) · gap(1) · name(18) · gauge · " NNNN%"
    let row = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(18),
        Constraint::Min(6),
        Constraint::Length(6),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(vec![mute_sym])), row[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(fit_name(label, 18), name_style)),
        row[2],
    );
    let gauge_color = if sink.muted {
        Color::DarkGray
    } else if selected {
        Color::Yellow
    } else {
        Color::Cyan
    };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
        .ratio(ratio)
        .label("");
    frame.render_widget(gauge, row[3]);
    frame.render_widget(
        Paragraph::new(format!(" {pct:4}%")).style(Style::default().fg(Color::Gray)),
        row[4],
    );
}

/// Truncate a volume-row label to `max` cells, appending `…` when cut, so long
/// application/device names don't spill into the gauge. Shared by both panels.
fn fit_name(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        format!("{}…", s.chars().take(keep).collect::<String>())
    }
}

fn render_app_row(
    app: &App,
    frame: &mut Frame,
    stream: &AppStream,
    idx: usize,
    area: Rect,
    panel_focused: bool,
) {
    let selected = panel_focused && app.app_cursor == idx;
    let name_style = if selected {
        Style::default().fg(Color::Yellow).bold()
    } else if stream.muted {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let mute_sym = if stream.muted {
        Span::styled("○", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled("●", Style::default().fg(Color::Green))
    };
    let pct = (stream.volume * 100.0).round() as i32;
    let ratio = stream.volume.clamp(0.0, 1.0);

    // Layout: mute(1) · gap(1) · name(18) · gauge(min) · " NNNN%"(6)
    let row = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(18),
        Constraint::Min(6),
        Constraint::Length(6),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(vec![mute_sym])), row[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(fit_name(&stream.display_name, 18), name_style)),
        row[2],
    );
    let gauge_color = if stream.muted {
        Color::DarkGray
    } else if selected {
        Color::Yellow
    } else {
        Color::Cyan
    };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
        .ratio(ratio)
        .label("");
    frame.render_widget(gauge, row[3]);
    frame.render_widget(
        Paragraph::new(format!(" {pct:4}%")).style(Style::default().fg(Color::Gray)),
        row[4],
    );
}

// ── EQ bands panel ────────────────────────────────────────────────────────

/// Render a single text cell into `rect`. Shared by the band header and data
/// rows so every cell aligns and styles identically.
fn put_cell(frame: &mut Frame, rect: Rect, text: &str, style: Style, align: Alignment) {
    frame.render_widget(Paragraph::new(text).style(style).alignment(align), rect);
}

/// Header-row column labels, chosen by available width: long words when the
/// table is wide, terse abbreviations when narrow. (`h_bar` is the gain-graph
/// column header, blank in the narrow layout.)
struct BandHeaderLabels {
    freq: &'static str,
    gain: &'static str,
    enabled: &'static str,
    bar: &'static str,
}

impl BandHeaderLabels {
    /// Pick labels for the wide (`full_names`) or narrow layout.
    fn for_width(full_names: bool) -> Self {
        if full_names {
            Self {
                freq: "Freq",
                gain: "Gain",
                enabled: "Enabled",
                bar: "Gain Graph",
            }
        } else {
            Self {
                freq: "Hz",
                gain: "dB",
                enabled: "On",
                bar: "",
            }
        }
    }
}

/// The two layout-dependent column indices that aren't positional constants:
/// the (optional) per-channel column and the always-last gain-bar column.
struct BandColumnIndices {
    /// Per-band channel column — present only on >2-channel devices.
    ch: Option<usize>,
    /// Gain-bar column — always the last rect from `band_columns`.
    bar: usize,
}

impl BandColumnIndices {
    /// Resolve from a `band_columns` split. The channel column sits at index 7
    /// (when shown); the gain bar is always the final rect.
    fn resolve(n_cols: usize, show_ch: bool) -> Self {
        Self {
            ch: show_ch.then_some(7usize),
            bar: n_cols - 1,
        }
    }
}

/// The band-table title with a "▸ Field" hint pointing at the active edit field
/// (only when the panel is focused).
fn band_panel_title(focused: bool, field: BandField) -> String {
    let field_hint = if focused {
        match field {
            BandField::Freq => " ▸ Freq",
            BandField::Gain => " ▸ Gain",
            BandField::Q => " ▸ Q",
        }
    } else {
        ""
    };
    format!(" EQ Bands{field_hint} ")
}

/// On/off glyph for a band's enabled state — wordy when the table is wide,
/// a bare bullet when narrow.
fn band_enable_label(enabled: bool, full_names: bool) -> &'static str {
    match (enabled, full_names) {
        (true, true) => "● On",
        (false, true) => "○ Off",
        (true, false) => "●",
        (false, false) => "○",
    }
}

fn render_bands(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Panel::Bands;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(
            Line::from(band_panel_title(focused, app.band_field)).fg(if focused {
                Color::Cyan
            } else {
                Color::Magenta
            }),
        )
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (bands, channels, layout) = match &app.state {
        Some(s) if !s.bands.is_empty() => (s.bands.clone(), s.channels, s.channel_layout.clone()),
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

    // Progressive disclosure: the per-band channel column only appears on
    // >2-channel devices so stereo users get a clean table.
    let show_ch = channels > 2;
    let full_names = crate::layout::band_type_full(inner.width);

    let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    render_band_header(app, frame, header_rect, focused, show_ch, full_names);

    // ── Data rows (with scroll) ──
    let visible = (inner.height - 1) as usize;
    let offset = crate::layout::band_scroll_offset(app.band_cursor, bands.len(), visible);

    for (vis, i) in (offset..bands.len()).take(visible).enumerate() {
        let y = inner.y + 1 + vis as u16;
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let selected = focused && app.band_cursor == i;
        render_band_row(
            app, frame, row_rect, i, &bands[i], selected, show_ch, full_names, &layout, channels,
        );
    }
}

/// Render the band-table header row: the static `#`/`Type`/`Q` labels plus the
/// width-dependent freq/gain/enabled/bar labels, with the active edit field's
/// header highlighted yellow.
fn render_band_header(
    app: &App,
    frame: &mut Frame,
    header_rect: Rect,
    focused: bool,
    show_ch: bool,
    full_names: bool,
) {
    let hcols = crate::layout::band_columns(header_rect, show_ch);
    let idx = BandColumnIndices::resolve(hcols.len(), show_ch);

    let hdr = Style::default().bold().fg(Color::DarkGray);
    let active_hdr = Style::default().bold().fg(Color::Yellow);
    let field_hdr = |field: BandField| {
        if focused && app.band_field == field {
            active_hdr
        } else {
            hdr
        }
    };

    let labels = BandHeaderLabels::for_width(full_names);
    put_cell(frame, hcols[0], "#", hdr, Alignment::Right);
    put_cell(frame, hcols[1], "Type", hdr, Alignment::Left);
    put_cell(
        frame,
        hcols[2],
        labels.freq,
        field_hdr(BandField::Freq),
        Alignment::Right,
    );
    put_cell(
        frame,
        hcols[3],
        labels.gain,
        field_hdr(BandField::Gain),
        Alignment::Right,
    );
    put_cell(
        frame,
        hcols[4],
        "Q",
        field_hdr(BandField::Q),
        Alignment::Center,
    );
    put_cell(frame, hcols[6], labels.enabled, hdr, Alignment::Center);
    if let Some(ci) = idx.ch {
        put_cell(frame, hcols[ci], "Ch", hdr, Alignment::Left);
    }
    put_cell(frame, hcols[idx.bar], labels.bar, hdr, Alignment::Center);
}

/// Render one band's data row: index, type, freq, gain, Q, enable, optional
/// channel tag, and the gain bar. The active edit field is drawn inverse, a
/// disabled band greys out, and the selected row gets a subtle background
/// stripe + bold cells.
#[allow(clippy::too_many_arguments)]
fn render_band_row(
    app: &App,
    frame: &mut Frame,
    row_rect: Rect,
    i: usize,
    b: &resonance_ipc::BandState,
    selected: bool,
    show_ch: bool,
    full_names: bool,
    layout: &[String],
    channels: usize,
) {
    let cols = crate::layout::band_columns(row_rect, show_ch);
    let idx = BandColumnIndices::resolve(cols.len(), show_ch);
    let bar_rect = cols[idx.bar];

    // Subtle stripe on the selected row so it reads as a row, not a cell.
    if selected {
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(28, 28, 36))),
            row_rect,
        );
    }

    // Active field → strong highlight; disabled band → grey; else colour by
    // meaning. Selected rows are bold.
    let field_style = Style::default().fg(Color::Black).bg(Color::Yellow).bold();
    let cell = |fg: Color| {
        let s = if b.enabled {
            Style::default().fg(fg)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        if selected { s.bold() } else { s }
    };
    let active = |field: BandField| selected && app.band_field == field;
    let field_or = |field: BandField, fg: Color| {
        if active(field) { field_style } else { cell(fg) }
    };

    // Type cell; for shelves + HP/LP append the filter slope (12/24/48 dB/oct),
    // which only those types honour. Single-biquad types show the type alone.
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

    put_cell(
        frame,
        cols[0],
        &format!("{}", i + 1),
        cell(Color::Gray),
        Alignment::Right,
    );
    put_cell(
        frame,
        cols[1],
        &type_name,
        cell(Color::Cyan),
        Alignment::Left,
    );
    put_cell(
        frame,
        cols[2],
        &fmt_freq(b.freq),
        field_or(BandField::Freq, freq_color(b.freq)),
        Alignment::Right,
    );
    put_cell(
        frame,
        cols[3],
        &format!("{:+.1}", b.gain_db),
        field_or(BandField::Gain, gain_color(b.gain_db)),
        Alignment::Right,
    );
    put_cell(
        frame,
        cols[4],
        &format!("{:.2}", b.q),
        field_or(BandField::Q, Color::Gray),
        Alignment::Right,
    );
    put_cell(
        frame,
        cols[6],
        band_enable_label(b.enabled, full_names),
        cell(Color::Green),
        Alignment::Center,
    );
    if let Some(ci) = idx.ch {
        let tag = channel_tag(b.channels, layout, channels);
        // Global (the common default) reads as dim "no override"; a real
        // subset stands out in cyan.
        let col = if b.channels.is_global(channels) {
            Color::DarkGray
        } else {
            Color::Cyan
        };
        put_cell(frame, cols[ci], &tag, cell(col), Alignment::Left);
    }
    let bar = gain_bar(b.gain_db, bar_rect.width as usize);
    put_cell(
        frame,
        bar_rect,
        &bar,
        cell(gain_color(b.gain_db)),
        Alignment::Left,
    );
}

/// Short label for a band's channel target: `all` / `FL` / `FL FR` / `FL +2`
/// / `none`. Mirrors the GUI's per-band channel tag (multichannel only).
fn channel_tag(mask: ChannelMask, layout: &[String], channels: usize) -> String {
    if mask.is_global(channels) {
        return "all".to_string();
    }
    let names: Vec<&str> = (0..channels)
        .filter(|&c| mask.contains(c))
        .map(|c| layout.get(c).map_or("?", String::as_str))
        .collect();
    match names.len() {
        0 => "none".to_string(),
        1 | 2 => names.join(" "),
        _ => format!("{} +{}", names[0], names.len() - 1),
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
    // Widen the multiply: area.width * pct_w overflows u16 past ~819 columns.
    let w = (u32::from(area.width) * u32::from(pct_w) / 100) as u16;
    let h = (u32::from(area.height) * u32::from(pct_h) / 100) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn render_browser(b: &Browser, frame: &mut Frame, area: Rect) {
    let dialog = centered_rect(area, 80, 80);
    frame.render_widget(Clear, dialog);

    let cwd = b.cwd.display().to_string();
    let verb = match b.purpose {
        crate::browser::BrowsePurpose::LoadPreset => "Load Preset",
        crate::browser::BrowsePurpose::LoadMeasurement => "Load Measurement",
    };
    let block = Block::default()
        .title(Line::from(format!(" {verb} — {cwd} ")).fg(Color::Yellow))
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
            let is_active = active.is_some_and(|a| a == s);
            let name = app
                .state
                .as_ref()
                .map_or_else(|| s.clone(), |st| st.sink_label(s));
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

// ── Per-band channel-target picker ──────────────────────────────────────────

fn render_band_channels(
    index: usize,
    mask: ChannelMask,
    cursor: usize,
    app: &App,
    frame: &mut Frame,
    area: Rect,
) {
    let (channels, layout) = app
        .state
        .as_ref()
        .map_or((0, vec![]), |s| (s.channels, s.channel_layout.clone()));

    // Grow to fit the channel list, but never past the screen: clamp the height
    // to `area` and recentre so the bottom border + keybind footer stay visible.
    // The inner List then scrolls (ListState keeps the cursor in view) for high
    // channel counts instead of rendering rows off-screen.
    let base = centered_rect(area, 44, 60);
    let min_h = (channels as u16 + 4).max(6);
    let w = base.width.max(28).min(area.width);
    let h = base.height.max(min_h).min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let dialog = Rect::new(x, y, w, h);
    frame.render_widget(Clear, dialog);

    let block = Block::default()
        .title(Line::from(format!(" Band {} channels ", index + 1)).fg(Color::Yellow))
        .title_bottom(
            Line::from(" ↑↓ move  Space toggle  a all  n none  Enter ok  Esc cancel ")
                .fg(Color::DarkGray),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    if channels == 0 {
        frame.render_widget(
            Paragraph::new(" (no channels)").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = (0..channels)
        .map(|c| {
            let name = layout.get(c).map_or("?", String::as_str);
            // A global (ALL) mask shows every channel as selected.
            let on = mask.is_global(channels) || mask.contains(c);
            let check = if on { "[x]" } else { "[ ]" };
            let style = if on {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(format!("{check} {name}")).style(style)
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(cursor.min(channels - 1)));
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().fg(Color::Yellow).bold());
    frame.render_stateful_widget(list, inner, &mut list_state);
}

// ── squig.link online browser ───────────────────────────────────────────────

fn render_squig_browse(
    tab: SquigTab,
    query: &str,
    cursor: usize,
    app: &App,
    frame: &mut Frame,
    area: Rect,
) {
    let dialog = centered_rect(area, 78, 80);
    frame.render_widget(Clear, dialog);

    let which = match tab {
        SquigTab::Models => "Measurements",
        SquigTab::Targets => "Targets",
    };
    let block = Block::default()
        .title(Line::from(format!(" Browse squig.link — {which} ")).fg(Color::Yellow))
        .title_bottom(
            Line::from(
                " type=search  ↑↓ move  Enter load  Tab models/targets  F5/^R refresh  Esc close ",
            )
            .fg(Color::DarkGray),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let rows = Layout::vertical([
        Constraint::Length(1), // search
        Constraint::Length(1), // status
        Constraint::Min(1),    // list
    ])
    .split(inner);

    // Search line (with a fake caret) + a busy spinner.
    let busy = if app.dl_busy { "  ⟳ loading…" } else { "" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("search ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{query}█"), Style::default().fg(Color::White)),
            Span::styled(busy, Style::default().fg(Color::Cyan)),
        ])),
        rows[0],
    );

    // Status line (worker messages, or a warming hint before the catalog lands).
    let status = if !app.dl_status.is_empty() {
        app.dl_status.clone()
    } else if app.catalog.is_none() {
        "warming catalog from squig.link…".to_string()
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(format!(" {status}")).style(Style::default().fg(Color::DarkGray)),
        rows[1],
    );

    let Some(cat) = &app.catalog else {
        frame.render_widget(
            Paragraph::new(" (loading squig.link catalog…)")
                .style(Style::default().fg(Color::DarkGray)),
            rows[2],
        );
        return;
    };

    let items: Vec<ListItem> = match tab {
        SquigTab::Models => crate::app::squig_filter_models(cat, query)
            .iter()
            .map(|m| {
                ListItem::new(Line::from(vec![
                    Span::styled(m.display.clone(), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("  · {}", m.source),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect(),
        SquigTab::Targets => crate::app::squig_filter_targets(cat, query)
            .iter()
            .map(|t| {
                ListItem::new(Line::from(vec![
                    Span::styled(t.name.clone(), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("  · {}", t.source),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect(),
    };

    if items.is_empty() {
        let msg = if cat.models.is_empty() && cat.targets.is_empty() {
            " (catalog empty — try ^R to refresh)"
        } else {
            " (no matches for the search)"
        };
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
            rows[2],
        );
        return;
    }

    let mut st = ListState::default();
    st.select(Some(cursor.min(items.len() - 1)));
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(Style::default().fg(Color::Yellow).bold());
    frame.render_stateful_widget(list, rows[2], &mut st);
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
    let base = " [Tab/←→/1-6] switch  [↑↓] select  [Esc] close";
    let ctx = match s.tab {
        0 => "  •  [Enter] load  [n] save  [e] export  [r] rename  [d] delete",
        1 => "  •  [m] map  [d] unmap",
        2 => "  •  [Enter] route  [m] map to profile",
        3 => "  •  [Enter/Space] edit/toggle",
        4 => "  •  [Enter] run action",
        5 => "  •  [Enter] act  [+/-] customizer",
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
        5 => render_tab_reference(s, app, frame, area),
        _ => {}
    }
}

fn render_tab_reference(s: &SettingsState, app: &App, frame: &mut Frame, area: Rect) {
    let r = &app.reference;
    let on = |b: bool| if b { "[on] " } else { "[off]" };
    let meas = match (&r.measurement, r.measurement_name.is_empty()) {
        (Some(_), false) => r.measurement_name.clone(),
        (Some(_), true) => "(loaded)".to_string(),
        (None, _) => "none".to_string(),
    };
    let autoeq = if app.autoeq_busy {
        "fitting…".to_string()
    } else {
        "fit measurement → target".to_string()
    };
    let rows: [(&str, String, &str); 13] = [
        (
            "Reference",
            on(r.enabled).to_string(),
            "(overlay on the FR graph; needs a measurement)",
        ),
        (
            "Target",
            r.target_label(),
            "(Enter cycles the target curve)",
        ),
        ("Measurement", meas, "(Enter to load a freq/dB .txt)"),
        (
            "Browse online",
            "squig.link".to_string(),
            "(Enter: search + load measurements/targets)",
        ),
        ("Auto-EQ", autoeq, "(Enter fits a 10-band correction)"),
        (
            "Show raw measurement",
            on(r.show_measurement).to_string(),
            "(draw the un-EQ'd curve too)",
        ),
        (
            "Normalize",
            on(r.normalized).to_string(),
            "(re-baseline onto the target → flat 0)",
        ),
        (
            "Preference bounds",
            on(r.show_bounds).to_string(),
            "(shade the listener-tolerance band)",
        ),
        // Customizer: stacks tilt/bass/ear/treble onto the active target.
        (
            "Tilt",
            format!("{:+.1} dB/oct", r.adj_tilt),
            "([+/-] tilt the target)",
        ),
        (
            "Bass",
            format!("{:+.1} dB", r.adj_bass),
            "([+/-] bass shelf)",
        ),
        (
            "Ear gain",
            format!("{:+.1} dB", r.adj_ear),
            "([+/-] ear gain)",
        ),
        (
            "Treble",
            format!("{:+.1} dB", r.adj_treble),
            "([+/-] treble shelf)",
        ),
        (
            "Reset customizer",
            String::new(),
            "(Enter zeroes tilt/bass/ear/treble)",
        ),
    ];

    for (i, (label, value, desc)) in rows.iter().enumerate() {
        let y = area.y + i as u16;
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
            Span::styled(format!("{marker} {label:<22} "), label_style),
            Span::styled(value.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(format!("  {desc}"), Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), row);
    }

    // Hint when enabled but nothing to show yet (no measurement → inactive).
    if r.enabled && !r.active() {
        let y = area.y + rows.len() as u16 + 1;
        if y < area.y + area.height {
            frame.render_widget(
                Paragraph::new("  load a measurement to see the overlay")
                    .style(Style::default().fg(Color::DarkGray).italic()),
                Rect::new(area.x, y, area.width, 1),
            );
        }
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
        Paragraph::new(" [Enter] load  [n] save current  [e] export  [r] rename  [d] delete")
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

    let active_label = active_output.map_or_else(
        || "none".to_string(),
        |o| {
            app.state
                .as_ref()
                .map_or_else(|| o.to_string(), |st| st.sink_label(o))
        },
    );
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
                .map_or_else(|| out.clone(), |st| st.sink_label(out));
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
                .map_or_else(|| sink.clone(), |st| st.sink_label(sink));
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
    let items: [(&str, String, &str); 6] = [
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

    // Prefer at least 30 columns, but never exceed the parent — clamp(30, max)
    // panics when the terminal is narrower than ~34 columns (min > max).
    let desired = (label.len() as u16)
        .saturating_add(display.len() as u16)
        .saturating_add(8);
    let max_w = parent.width.saturating_sub(4).max(1);
    let w = desired.max(30).min(max_w);
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
        format!("{hz:.0}Hz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resonance_ipc::default_channel_layout;

    fn layout6() -> Vec<String> {
        default_channel_layout(6) // FL FR FC LFE RL RR
    }

    #[test]
    fn channel_tag_global_is_all() {
        assert_eq!(channel_tag(ChannelMask::ALL, &layout6(), 6), "all");
        // A concrete full set also reads as "all".
        assert_eq!(
            channel_tag(ChannelMask::from_indices(0..6), &layout6(), 6),
            "all"
        );
    }

    #[test]
    fn channel_tag_empty_is_none() {
        assert_eq!(channel_tag(ChannelMask::NONE, &layout6(), 6), "none");
    }

    #[test]
    fn channel_tag_single_and_pair_name_channels() {
        assert_eq!(channel_tag(ChannelMask::single(0), &layout6(), 6), "FL");
        assert_eq!(
            channel_tag(ChannelMask::from_indices([0, 1]), &layout6(), 6),
            "FL FR"
        );
    }

    #[test]
    fn channel_tag_many_summarises_with_count() {
        // 3+ channels collapse to "<first> +N".
        assert_eq!(
            channel_tag(ChannelMask::from_indices([0, 2, 4]), &layout6(), 6),
            "FL +2"
        );
    }

    #[test]
    fn channel_tag_unknown_layout_index_is_placeholder() {
        // A non-global mask targeting a channel with no layout label → "?".
        assert_eq!(
            channel_tag(ChannelMask::single(1), &["FL".to_string()], 2),
            "?"
        );
    }

    // ── Headless render smoke tests (TestBackend) ──────────────────────────

    use crate::app::{App, InputMode, Panel, SquigTab};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use resonance_ipc::{AppStream, BandState, BandType, DaemonState, EffectsState, Meters};

    fn app_stream(key: &str, name: &str, volume: f64, muted: bool) -> AppStream {
        AppStream {
            key: key.into(),
            display_name: name.into(),
            pid: Some(1),
            volume,
            muted,
            active: true,
        }
    }

    fn sink_volume(name: &str, desc: &str, volume: f64, muted: bool) -> SinkVolume {
        SinkVolume {
            name: name.into(),
            description: desc.into(),
            volume,
            muted,
        }
    }

    fn band(freq: f64, channels: ChannelMask) -> BandState {
        BandState {
            band_type: BandType::Peaking,
            freq,
            gain_db: 3.0,
            q: 1.4,
            enabled: true,
            channels,
            slope_db_oct: 12,
            scope: resonance_ipc::BandScope::Stereo,
        }
    }

    fn fixture(channels: usize) -> DaemonState {
        DaemonState {
            enabled: true,
            preamp_db: 0.0,
            eq_enabled: true,
            bands: vec![
                band(100.0, ChannelMask::ALL),
                band(1000.0, ChannelMask::single(2)), // targets FC on ≥3ch
            ],
            effects: EffectsState::default(),
            current_preset: None,
            sample_rate: 48000.0,
            capture_rate: 48000.0,
            channels,
            out_channels: channels,
            channel_layout: default_channel_layout(channels),
            routing: None,
            spectrum: vec![0.0; 16],
            active_output: None,
            mapped_profile: None,
            available_sinks: vec![],
            sink_descriptions: vec![],
            preferred_output: None,
            meters: Meters::default(),
            apps: vec![],
            sinks: vec![],
            dither_bits: None,
        }
    }

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn render_to_text(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(app, f)).unwrap();
        buffer_text(term.backend().buffer())
    }

    #[test]
    fn multichannel_shows_channel_column_and_status() {
        let mut app = App::new();
        app.state = Some(fixture(6));
        app.focus = Panel::Bands;
        let text = render_to_text(&app, 120, 44);
        // Ch column header + the per-band FC tag are present on >2ch.
        assert!(text.contains(" Ch "), "missing Ch header:\n{text}");
        assert!(text.contains("FC"), "missing FC channel tag:\n{text}");
        // Channel summary surfaces in the status bar.
        assert!(text.contains("6ch"), "missing 6ch status:\n{text}");
    }

    #[test]
    fn stereo_hides_channel_column() {
        let mut app = App::new();
        app.state = Some(fixture(2));
        app.focus = Panel::Bands;
        let text = render_to_text(&app, 120, 44);
        // Progressive disclosure: no Ch column, no >2ch position labels, and no
        // channel summary in the status (plain stereo passthrough).
        assert!(!text.contains("FC"), "stereo leaked a >2ch label:\n{text}");
        assert!(
            !text.contains("2ch"),
            "stereo showed a channel summary:\n{text}"
        );
    }

    #[test]
    fn applications_panel_lists_apps_when_present() {
        let mut app = App::new();
        app.prefs.show_apps = true; // decouple from on-disk prefs
        let mut st = fixture(2);
        st.apps.push(app_stream("firefox.1", "Firefox", 1.0, false));
        st.apps.push(app_stream("spotify.2", "Spotify", 0.5, true));
        app.state = Some(st);
        let text = render_to_text(&app, 120, 44);
        assert!(
            text.contains("Applications"),
            "missing Applications panel:\n{text}"
        );
        assert!(text.contains("Firefox"), "missing Firefox row:\n{text}");
        assert!(text.contains("Spotify"), "missing Spotify row:\n{text}");
    }

    #[test]
    fn applications_panel_hidden_without_apps() {
        let mut app = App::new();
        app.prefs.show_apps = true; // toggle on, but no apps → still hidden
        app.state = Some(fixture(2)); // fixture has no apps
        let text = render_to_text(&app, 120, 44);
        assert!(
            !text.contains("Applications"),
            "Applications panel shown with no apps:\n{text}"
        );
    }

    #[test]
    fn tab_includes_apps_only_when_present() {
        let mut app = App::new();
        // Decouple from on-disk prefs: the panel-visibility toggles gate Tab now.
        app.prefs.show_apps = true;
        app.prefs.show_sinks = false;
        // No apps: Graph wraps straight back to Effects.
        app.state = Some(fixture(2));
        app.focus = Panel::Graph;
        app.next_panel();
        assert_eq!(app.focus, Panel::Effects, "no-apps Graph should skip Apps");
        // With apps: Graph → Apps → Effects.
        let mut st = fixture(2);
        st.apps.push(app_stream("firefox.1", "Firefox", 1.0, false));
        app.state = Some(st);
        app.focus = Panel::Graph;
        app.next_panel();
        assert_eq!(app.focus, Panel::Apps, "Graph should advance to Apps");
        app.next_panel();
        assert_eq!(app.focus, Panel::Effects, "Apps should wrap to Effects");
    }

    #[test]
    fn outputs_panel_toggle_gates_visibility_and_tab() {
        let mut app = App::new();
        app.prefs.show_apps = false;
        app.prefs.show_sinks = false;
        let mut st = fixture(2);
        st.sinks
            .push(sink_volume("dev.uid", "Built-in Speakers", 0.85, false));
        app.state = Some(st);
        // Toggle off → not visible → Graph skips Outputs.
        assert!(!app.sinks_visible(), "hidden while toggle off");
        app.focus = Panel::Graph;
        app.next_panel();
        assert_eq!(
            app.focus,
            Panel::Effects,
            "Graph should skip hidden Outputs"
        );
        // Toggle on → visible + in the Tab cycle after Graph.
        app.toggle_sinks_panel();
        assert!(app.sinks_visible(), "visible after toggle + sinks present");
        app.focus = Panel::Graph;
        app.next_panel();
        assert_eq!(app.focus, Panel::Sinks, "Graph should advance to Outputs");
        // Rendered panel shows the device label.
        let text = render_to_text(&app, 120, 44);
        assert!(text.contains("Outputs"), "Outputs title missing:\n{text}");
        assert!(
            text.contains("Built-in Speakers"),
            "device label missing:\n{text}"
        );
    }

    #[test]
    fn channel_picker_renders_checkboxes() {
        let mut app = App::new();
        app.state = Some(fixture(6));
        app.focus = Panel::Bands;
        app.mode = InputMode::SelectBandChannels {
            index: 1,
            mask: ChannelMask::single(2),
            cursor: 0,
        };
        let text = render_to_text(&app, 120, 44);
        assert!(
            text.contains("Band 2 channels"),
            "missing picker title:\n{text}"
        );
        assert!(text.contains("[x]"), "missing checked box:\n{text}");
        assert!(text.contains("[ ]"), "missing unchecked box:\n{text}");
    }

    #[test]
    fn channel_picker_fits_small_terminal_with_many_channels() {
        let mut app = App::new();
        app.state = Some(fixture(64));
        app.focus = Panel::Bands;
        app.mode = InputMode::SelectBandChannels {
            index: 0,
            mask: ChannelMask::ALL,
            cursor: 60,
        };
        // 24-row terminal with 64 channels: the dialog must clamp to the screen
        // so the bottom border + keybind footer stay visible (and the inner list
        // scrolls instead of rendering rows off-screen). No panic either.
        let text = render_to_text(&app, 100, 24);
        // The footer row is on-screen (its text may be horizontally truncated by
        // the dialog width, like any ratatui title); the title shows up top; and
        // the list scrolled to keep the cursor channel visible (not off-screen).
        assert!(
            text.contains("↑↓ move"),
            "picker footer row clipped:\n{text}"
        );
        assert!(
            text.contains("Band 1 channels"),
            "picker title clipped:\n{text}"
        );
        assert!(
            text.contains("CH60"),
            "list did not scroll to cursor:\n{text}"
        );
    }

    #[test]
    fn reference_tab_renders_controls() {
        let mut app = App::new();
        app.state = Some(fixture(2));
        let mut ss = crate::settings::SettingsState::new(vec![], vec![], vec![]);
        ss.tab = 5; // Reference
        app.mode = InputMode::Settings(ss);
        let text = render_to_text(&app, 120, 44);
        for needle in [
            "Reference",
            "Target",
            "Measurement",
            "Browse online",
            "Auto-EQ",
            "Normalize",
            "Preference bounds",
            "Tilt",
            "Bass",
            "Treble",
            "Reset customizer",
        ] {
            assert!(
                text.contains(needle),
                "reference tab missing {needle}:\n{text}"
            );
        }
    }

    // float_cmp: asserts adj_tilt is unchanged vs a stored snapshot (exact, no math).
    #[allow(clippy::float_cmp)]
    #[test]
    fn reference_customizer_adjust_is_reference_tab_only() {
        let mut app = App::new();
        app.state = Some(fixture(2));
        let mut ss = crate::settings::SettingsState::new(vec![], vec![], vec![]);
        ss.tab = 5;
        ss.cursor = 8; // Tilt (Browse online at row 3 shifted customizer to 8–11)
        app.mode = InputMode::Settings(ss);
        app.settings_adjust(1.0);
        assert!(app.reference.adj_tilt > 0.0, "tilt should increase");
        // +/- is a no-op on a non-Reference tab.
        if let InputMode::Settings(s) = &mut app.mode {
            s.tab = 3;
            s.cursor = 0;
        }
        let before = app.reference.adj_tilt;
        app.settings_adjust(1.0);
        assert_eq!(
            app.reference.adj_tilt, before,
            "customizer adjust only applies on the Reference tab"
        );
    }

    #[test]
    fn squig_filter_matches_display_and_source() {
        use resonance_reference::download::{Catalog, ModelEntry};
        let m = |source: &str, display: &str| ModelEntry {
            source: source.into(),
            display: display.into(),
            file: "f".into(),
            base_url: String::new(),
            kind: String::new(),
        };
        let cat = Catalog {
            sources: vec![],
            models: vec![
                m("dhrme", "Sennheiser HD600"),
                m("precog", "Moondrop Blessing"),
            ],
            targets: vec![],
        };
        assert_eq!(crate::app::squig_filter_models(&cat, "moon").len(), 1);
        assert_eq!(crate::app::squig_filter_models(&cat, "dhrme").len(), 1); // by source
        assert_eq!(crate::app::squig_filter_models(&cat, "").len(), 2);
        assert_eq!(crate::app::squig_filter_models(&cat, "zzz").len(), 0);
    }

    #[test]
    fn squig_browse_renders_catalog() {
        use resonance_reference::download::{Catalog, ModelEntry, TargetEntry};
        let mut app = App::new();
        app.state = Some(fixture(2));
        app.catalog = Some(Catalog {
            sources: vec![],
            models: vec![ModelEntry {
                source: "dhrme".into(),
                display: "Sennheiser HD600".into(),
                file: "HD600".into(),
                base_url: "https://x/".into(),
                kind: "Headphones".into(),
            }],
            targets: vec![TargetEntry {
                source: "dhrme".into(),
                name: "Harman 2019".into(),
                base_url: "https://x/".into(),
            }],
        });
        app.mode = InputMode::SquigBrowse {
            tab: SquigTab::Models,
            query: String::new(),
            cursor: 0,
        };
        let text = render_to_text(&app, 120, 44);
        assert!(text.contains("Browse squig.link"), "no title:\n{text}");
        assert!(text.contains("HD600"), "model not listed:\n{text}");
        // The Targets tab lists target curves instead.
        app.mode = InputMode::SquigBrowse {
            tab: SquigTab::Targets,
            query: String::new(),
            cursor: 0,
        };
        let text2 = render_to_text(&app, 120, 44);
        assert!(text2.contains("Harman 2019"), "target not listed:\n{text2}");
    }

    #[test]
    fn hiding_spectrum_enlarges_the_graph() {
        let area = ratatui::layout::Rect::new(0, 0, 100, 44);
        let on = crate::layout::panes(area, true, false, false);
        let off = crate::layout::panes(area, false, false, false);
        assert!(
            off.eq.height > on.eq.height,
            "graph should grow when the spectrum is hidden ({} → {})",
            on.eq.height,
            off.eq.height
        );
        assert_eq!(off.spectrum.height, 0, "hidden spectrum takes no rows");
    }

    // float_cmp: asserts the JSON round-trip preserves the exact 3.0 literal.
    #[allow(clippy::float_cmp)]
    #[test]
    fn reference_persists_as_json_round_trip() {
        use resonance_reference::reference::{PersistedReference, ReferenceState};
        let r = ReferenceState {
            enabled: true,
            adj_bass: 3.0,
            ..ReferenceState::default()
        };
        let p = r.to_persisted();
        // JSON must succeed — TOML would fail here (PersistedReference has scalar
        // fields after its table-valued Option<RefCurve> fields).
        let s = serde_json::to_string(&p).expect("reference persists as JSON");
        let p2: PersistedReference = serde_json::from_str(&s).unwrap();
        let mut r2 = ReferenceState::default();
        r2.restore(p2);
        assert!(r2.enabled);
        assert_eq!(r2.adj_bass, 3.0);
    }

    #[test]
    fn active_reference_overlay_renders_without_panic() {
        use resonance_ipc::curve::RefCurve;
        use resonance_reference::reference::TargetSel;
        let mut app = App::new();
        app.state = Some(fixture(2));
        // A flat measurement + a real target makes the reference overlay active.
        let flat = RefCurve::from_points(vec![(20.0, 0.0), (1000.0, 0.0), (20000.0, 0.0)]);
        app.reference
            .set_measurement("test".to_string(), false, flat, None);
        if let Some((_, sel)) = app
            .reference
            .target_options()
            .into_iter()
            .find(|(_, s)| *s != TargetSel::None)
        {
            app.reference.set_target(sel);
        }
        app.reference.enabled = true;
        app.reference.show_bounds = true; // exercise the tolerance-band path too
        assert!(app.reference.active(), "measurement + enabled → active");
        // Reference mode must produce the named target + result series that the
        // graph's legend is built from. Assert on the series MODEL, not on the
        // rendered legend text: the ratatui Chart legend auto-hides when it
        // doesn't fit the available space, and that space depends on layout/prefs
        // loaded from disk (e.g. `show_spectrum`), so a rendered-text check is
        // non-hermetic — it passes on a dev machine but fails on a clean CI box.
        let roles: Vec<SeriesRole> = app
            .reference
            .series(
                &[],
                48000.0,
                64,
                resonance_ipc::fr::LOG_MIN,
                resonance_ipc::fr::LOG_MAX,
                0.0,
            )
            .into_iter()
            .map(|s| s.role)
            .collect();
        assert!(roles.contains(&SeriesRole::Target), "no Target series");
        assert!(roles.contains(&SeriesRole::Result), "no Result series");
        // The overlay + bounds (shaded band) path must render without panicking,
        // both with the spectrum shown and hidden (different graph height).
        let text = render_to_text(&app, 120, 44);
        assert!(!text.trim().is_empty());
        app.prefs.show_spectrum = false;
        let text2 = render_to_text(&app, 120, 44);
        assert!(!text2.trim().is_empty());
    }

    #[test]
    fn graph_focus_title_shows_node_readout() {
        let mut app = App::new();
        app.state = Some(fixture(2));
        app.focus = Panel::Graph;
        app.band_cursor = 0;
        let text = render_to_text(&app, 120, 44);
        // Title shows the selected node's freq/gain in edit mode.
        assert!(text.contains("edit"), "no edit readout in title:\n{text}");
    }

    #[test]
    fn graph_pixel_data_round_trips() {
        use crate::layout::{eq_plot_area, graph_node_col, graph_node_row, graph_pixel_to_data};
        let plot = eq_plot_area(ratatui::layout::Rect::new(0, 0, 110, 22));
        // A node at 1 kHz / +6 dB maps to a cell and back to ~the same values
        // (within cell quantisation).
        let col = graph_node_col(plot, 1000.0);
        let row = graph_node_row(plot, 6.0);
        let (f, g) = graph_pixel_to_data(plot, col, row);
        assert!(
            (f.log10() - 1000f64.log10()).abs() < 0.05,
            "freq ~1k, got {f}"
        );
        assert!((g - 6.0).abs() < 2.0, "gain ~6, got {g}");
    }

    #[test]
    fn graph_press_selects_node_and_starts_drag() {
        let mut app = App::new();
        app.state = Some(fixture(2));
        app.last_frame = ratatui::layout::Rect::new(0, 0, 120, 44);
        app.graph_press(60, 20, false);
        assert_eq!(app.focus, Panel::Graph, "press focuses the graph");
        assert!(app.is_graph_dragging(), "press starts a drag");
        app.graph_release();
        assert!(!app.is_graph_dragging(), "release ends the drag");
    }

    #[test]
    fn graph_select_moves_and_clamps_cursor() {
        let mut app = App::new();
        app.state = Some(fixture(2)); // 2 bands
        app.band_cursor = 0;
        app.graph_select(1);
        assert_eq!(app.band_cursor, 1);
        app.graph_select(1); // clamp at the last band
        assert_eq!(app.band_cursor, 1);
        app.graph_select(-5); // clamp at 0
        assert_eq!(app.band_cursor, 0);
    }

    #[test]
    fn curve_runs_flat_is_single_neutral_run() {
        let pts = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.5)];
        let runs = curve_runs(&pts);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, Color::Cyan);
        assert_eq!(runs[0].1, pts);
    }

    #[test]
    fn curve_runs_split_by_sign_and_bridge_for_continuity() {
        // neutral, neutral, boost, boost, cut.
        let pts = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 5.0), (3.0, 5.0), (4.0, -5.0)];
        let runs = curve_runs(&pts);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].0, Color::Cyan);
        assert_eq!(runs[1].0, Color::Green);
        assert_eq!(runs[2].0, Color::LightRed);
        // Each run begins at the previous run's last point so the coloured
        // segments join with no visible gap.
        assert_eq!(runs[1].1[0], *runs[0].1.last().unwrap());
        assert_eq!(runs[2].1[0], *runs[1].1.last().unwrap());
        // Dropping the bridge point reconstructs the original point sequence.
        let mut rebuilt = runs[0].1.clone();
        rebuilt.extend(runs[1].1.iter().skip(1).copied());
        rebuilt.extend(runs[2].1.iter().skip(1).copied());
        assert_eq!(rebuilt, pts);
    }

    // ── Pure band-table / EQ-graph helpers ─────────────────────────────────

    #[test]
    fn band_panel_title_shows_field_hint_only_when_focused() {
        assert_eq!(band_panel_title(true, BandField::Gain), " EQ Bands ▸ Gain ");
        assert_eq!(band_panel_title(true, BandField::Q), " EQ Bands ▸ Q ");
        // Unfocused: no field hint.
        assert_eq!(band_panel_title(false, BandField::Gain), " EQ Bands ");
    }

    #[test]
    fn band_enable_label_varies_by_state_and_width() {
        assert_eq!(band_enable_label(true, true), "● On");
        assert_eq!(band_enable_label(false, true), "○ Off");
        assert_eq!(band_enable_label(true, false), "●");
        assert_eq!(band_enable_label(false, false), "○");
    }

    #[test]
    fn band_header_labels_switch_on_width() {
        let wide = BandHeaderLabels::for_width(true);
        assert_eq!(
            (wide.freq, wide.gain, wide.enabled, wide.bar),
            ("Freq", "Gain", "Enabled", "Gain Graph")
        );
        let narrow = BandHeaderLabels::for_width(false);
        assert_eq!(
            (narrow.freq, narrow.gain, narrow.enabled, narrow.bar),
            ("Hz", "dB", "On", "")
        );
    }

    #[test]
    fn band_column_indices_resolve() {
        // Channel column only when shown; gain bar is always the last rect.
        let with_ch = BandColumnIndices::resolve(9, true);
        assert_eq!(with_ch.ch, Some(7));
        assert_eq!(with_ch.bar, 8);
        let no_ch = BandColumnIndices::resolve(8, false);
        assert_eq!(no_ch.ch, None);
        assert_eq!(no_ch.bar, 7);
    }

    // float_cmp: marker mapping is a copy/clamp, not arithmetic — exact equality.
    #[allow(clippy::float_cmp)]
    #[test]
    fn band_marker_point_clamps_gain_to_range() {
        let mut b = band(1000.0, ChannelMask::ALL);
        b.gain_db = 99.0;
        let (x, y) = band_marker_point(&b);
        assert_eq!(x, curve::band_marker_x(1000.0));
        assert_eq!(y, DB_RANGE); // clamped from 99 dB to the chart edge
        b.gain_db = -99.0;
        assert_eq!(band_marker_point(&b).1, -DB_RANGE);
    }

    #[test]
    fn band_markers_split_selected_and_skip_disabled() {
        let mut bands = vec![
            band(100.0, ChannelMask::ALL),
            band(1000.0, ChannelMask::ALL),
            band(5000.0, ChannelMask::ALL),
        ];
        bands[2].enabled = false; // disabled bands are dropped from both sets
        let (others, selected) = band_markers(&bands, 1);
        assert_eq!(others.len(), 1, "only band 0 is an 'other' enabled marker");
        assert_eq!(selected.len(), 1, "band 1 is the selected marker");
        // No selection in range → no selected marker, all enabled bands are others.
        let (others, selected) = band_markers(&bands, usize::MAX);
        assert_eq!(others.len(), 2);
        assert!(selected.is_empty());
    }

    #[test]
    fn selected_guide_points_only_in_graph_focus() {
        let bands = vec![band(1000.0, ChannelMask::ALL)];
        assert!(selected_guide_points(&bands, 0, false).is_empty());
        let g = selected_guide_points(&bands, 0, true);
        assert_eq!(g.len(), 2, "a top-to-bottom vertical guide is two points");
        // Out-of-range cursor → no guide, even in graph focus.
        assert!(selected_guide_points(&bands, 9, true).is_empty());
    }

    #[test]
    fn eq_y_axis_labels_span_the_range() {
        let labels = eq_y_axis_labels();
        assert_eq!(labels.len(), 3);
        assert_eq!(labels[0].content, "-18");
        assert_eq!(labels[2].content, "+18");
    }

    // ── Status-bar pure helpers ─────────────────────────────────────────────

    #[test]
    fn sample_rate_label_collapses_matching_rates() {
        // Within the 1 Hz dead zone → a single rate, no arrow.
        assert_eq!(sample_rate_label(48000.0, 48000.0), "48000 Hz");
        assert_eq!(sample_rate_label(48000.5, 48000.0), "48000 Hz");
        // Resampling → capture→DSP.
        assert_eq!(sample_rate_label(44100.0, 48000.0), "44100→48000 Hz");
    }

    #[test]
    fn preamp_label_dead_zone_is_dim_zero() {
        let (text, color) = preamp_label(0.0);
        assert_eq!(text, "preamp 0 dB");
        assert_eq!(color, Color::DarkGray);
        // Just inside the ±0.05 dead zone still reads as 0.
        assert_eq!(preamp_label(0.04).0, "preamp 0 dB");
        // Outside → signed value, cyan.
        let (text, color) = preamp_label(-3.5);
        assert_eq!(text, "preamp -3.5 dB");
        assert_eq!(color, Color::Cyan);
        assert_eq!(preamp_label(3.5).0, "preamp +3.5 dB");
    }

    #[test]
    fn meter_db_is_inf_below_floor_and_fixed_width() {
        assert_eq!(meter_db(0.0), "-inf");
        assert_eq!(meter_db(1e-9), "-inf");
        // 0 dBFS (peak 1.0) and a fixed 4-char width.
        assert_eq!(meter_db(1.0), "  +0");
        // -6 dBFS ≈ 0.5.
        assert_eq!(meter_db(0.5), "  -6");
    }

    #[test]
    fn dsp_load_color_thresholds() {
        assert_eq!(dsp_load_color(0.0), Color::DarkGray);
        assert_eq!(dsp_load_color(0.5), Color::DarkGray);
        assert_eq!(dsp_load_color(0.6), Color::Yellow);
        assert_eq!(dsp_load_color(0.9), Color::Red);
    }

    #[test]
    fn channel_summary_hidden_for_plain_stereo() {
        // Plain 2→2 passthrough, no routing → nothing shown.
        assert_eq!(channel_summary(2, 2, None), None);
    }

    #[test]
    fn channel_summary_shows_count_and_routing() {
        // Multichannel passthrough → bare count.
        assert_eq!(channel_summary(6, 6, None).as_deref(), Some("6ch"));
        // Up/down-mix → in→out count.
        assert_eq!(channel_summary(2, 6, None).as_deref(), Some("2→6ch"));
        // A pure L/R swap is tagged distinctly from a general routing matrix.
        let swap = RoutingMatrix::swap(2, 0, 1);
        assert_eq!(
            channel_summary(2, 2, Some(&swap)).as_deref(),
            Some("2ch L⇄R")
        );
        // Any other routing reads as "routed".
        let id = RoutingMatrix::identity(2);
        assert_eq!(
            channel_summary(2, 2, Some(&id)).as_deref(),
            Some("2ch routed")
        );
    }
}
