//! Shared geometry for the main view. Both the renderer (`ui`) and the mouse
//! hit-testing (`app`) compute panel/cell rectangles from these helpers so a
//! click always lands on exactly what was drawn.

use ratatui::layout::{Constraint, Layout, Rect};
use std::rc::Rc;

/// Top-level panel rectangles (outer, i.e. including borders).
pub struct Panes {
    pub status: Rect,
    pub eq: Rect,
    pub spectrum: Rect,
    pub effects: Rect,
    pub bands: Rect,
    /// Per-application volume strip (zero-size when hidden).
    pub apps: Rect,
    /// Per-output-sink volume strip (zero-size when hidden).
    pub sinks: Rect,
    pub footer: Rect,
}

/// Split the full frame: status / EQ curve / spectrum on top (full width),
/// then Effects | Bands side by side, then a footer.
///
/// The EQ curve is the elastic hero (`Fill(3)` vs the controls' `Fill(2)`), so
/// it takes the lion's share of the height and grows with the window. The
/// spectrum is a fixed strip that collapses to nothing when `show_spectrum` is
/// off, handing its rows to the graph.
pub fn panes(area: Rect, show_spectrum: bool, show_apps: bool, show_sinks: bool) -> Panes {
    let spectrum_h = if show_spectrum { 13 } else { 0 };
    // Applications + Outputs share one fixed bordered row below the controls,
    // split side-by-side when both are shown (so two volume panels cost the same
    // vertical space as one) and collapsing to nothing when neither is.
    let extras_h = if show_apps || show_sinks { 7 } else { 0 };
    let v = Layout::vertical([
        Constraint::Length(1),          // status
        Constraint::Fill(3),            // EQ curve — the hero
        Constraint::Length(spectrum_h), // spectrum (0 = hidden)
        Constraint::Fill(2),            // bands + effects row
        Constraint::Length(extras_h),   // applications | outputs (0 = hidden)
        Constraint::Length(1),          // footer
    ])
    .split(area);

    // Bands on the left (wide), effects on the right (narrow).
    let bottom =
        Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)]).split(v[3]);

    // Split the extras row: both → side-by-side halves; one → full width.
    let (apps, sinks) = match (show_apps, show_sinks) {
        (true, true) => {
            let e = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(v[4]);
            (e[0], e[1])
        }
        (true, false) => (v[4], Rect::default()),
        (false, true) => (Rect::default(), v[4]),
        (false, false) => (Rect::default(), Rect::default()),
    };

    Panes {
        status: v[0],
        eq: v[1],
        spectrum: v[2],
        bands: bottom[0],
        effects: bottom[1],
        apps,
        sinks,
        footer: v[5],
    }
}

/// Inner area of a bordered block (1-cell border on each side).
pub fn block_inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// Evenly-distributed row rectangles for the vertical effects column.
pub fn effect_rows(inner: Rect, n: usize) -> Rc<[Rect]> {
    let constraints = vec![Constraint::Ratio(1, n as u32); n];
    Layout::vertical(constraints).split(inner)
}

/// Whether the band table is wide enough to show full filter-type names.
pub fn band_type_full(width: u16) -> bool {
    width >= 46
}

/// Column rectangles for one band row (or the header row): #, Type, Freq,
/// Gain, Q, Enable, an optional per-band channel-target column (multichannel
/// only), and a gain bar that absorbs the remaining width. The Type column
/// widens to fit full names on wide terminals.
///
/// Rect indices are stable for the fixed columns regardless of `show_ch`
/// (0=#, 1=Type, 2=Freq, 3=Gain, 4=Q, 5=spacer, 6=Enable). When `show_ch` the
/// Ch column is rect 7 and the gain bar follows it; otherwise the gain bar is
/// rect 7. The bar is always the last rect.
pub fn band_columns(row: Rect, show_ch: bool) -> Rc<[Rect]> {
    let type_w = if band_type_full(row.width) { 11 } else { 5 };
    let mut cons = vec![
        Constraint::Length(3),      // 0 #
        Constraint::Length(type_w), // 1 Type
        Constraint::Length(8),      // 2 Freq
        Constraint::Length(7),      // 3 Gain
        Constraint::Length(6),      // 4 Q
        Constraint::Length(2),      // 5 spacer (extra gap before Enable)
        Constraint::Length(7),      // 6 Enable
    ];
    if show_ch {
        cons.push(Constraint::Length(8)); // 7 Ch (per-band channel target)
    }
    cons.push(Constraint::Min(0)); // gain bar (fills the rest), always last
    Layout::horizontal(cons).spacing(1).split(row)
}

/// First band scroll offset so the cursor stays visible.
pub fn band_scroll_offset(cursor: usize, n: usize, visible: usize) -> usize {
    if visible == 0 || n <= visible {
        0
    } else if cursor >= visible {
        (cursor - visible + 1).min(n - visible)
    } else {
        0
    }
}

/// True if (col,row) is inside a rectangle.
pub fn hit(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

// ── EQ-graph geometry (for keyboard/mouse node editing) ──────────────────────

/// Half-range of the EQ-curve dB axis. Must match `ui::DB_RANGE` (the Chart's
/// y bounds are ±this), so pixel↔gain mapping lines up with the rendered curve.
pub const GRAPH_DB_RANGE: f64 = 18.0;

/// Log10 frequency bounds of the EQ curve's x-axis (20 Hz – 20 kHz), matching
/// `curve::x_axis_ticks()` so mapped positions line up with the rendered chart.
pub fn graph_log_range() -> (f64, f64) {
    (20f64.log10(), 20000f64.log10())
}

/// The interactive plotting rectangle inside the EQ-curve panel — the region a
/// ratatui `Chart` draws data into, i.e. the bordered inner area minus the
/// y-axis labels+line on the left and the x-axis line+labels at the bottom.
/// Best-effort (our y labels are 3 chars wide, +1 axis column; 2 bottom rows);
/// click/drag uses nearest-node selection, so a 1-cell offset stays usable.
pub fn eq_plot_area(eq: Rect) -> Rect {
    const LEFT: u16 = 4; // 3-char y labels ("-18"/" 0"/"+18") + axis column
    const BOTTOM: u16 = 2; // x-axis line row + label row
    let inner = block_inner(eq);
    Rect::new(
        inner.x.saturating_add(LEFT),
        inner.y,
        inner.width.saturating_sub(LEFT),
        inner.height.saturating_sub(BOTTOM),
    )
}

/// Map a cell (col,row) inside the plot to (freq Hz, gain dB), clamped to the
/// visible ranges.
pub fn graph_pixel_to_data(plot: Rect, col: u16, row: u16) -> (f64, f64) {
    let (lmin, lmax) = graph_log_range();
    let w = f64::from(plot.width.max(2) - 1);
    let h = f64::from(plot.height.max(2) - 1);
    let fx = (f64::from(col.saturating_sub(plot.x)) / w).clamp(0.0, 1.0);
    let fy = (f64::from(row.saturating_sub(plot.y)) / h).clamp(0.0, 1.0);
    let freq = 10f64.powf(lmin + fx * (lmax - lmin)).clamp(20.0, 20000.0);
    let gain = (GRAPH_DB_RANGE - fy * 2.0 * GRAPH_DB_RANGE).clamp(-GRAPH_DB_RANGE, GRAPH_DB_RANGE);
    (freq, gain)
}

/// Column where a band node at `freq` is drawn within the plot.
pub fn graph_node_col(plot: Rect, freq: f64) -> u16 {
    let (lmin, lmax) = graph_log_range();
    let w = f64::from(plot.width.max(2) - 1);
    let t = ((freq.clamp(20.0, 20000.0).log10() - lmin) / (lmax - lmin)).clamp(0.0, 1.0);
    plot.x + (t * w).round() as u16
}

/// Row where a band node at `gain` is drawn within the plot.
pub fn graph_node_row(plot: Rect, gain: f64) -> u16 {
    let h = f64::from(plot.height.max(2) - 1);
    let t = ((GRAPH_DB_RANGE - gain.clamp(-GRAPH_DB_RANGE, GRAPH_DB_RANGE))
        / (2.0 * GRAPH_DB_RANGE))
        .clamp(0.0, 1.0);
    plot.y + (t * h).round() as u16
}
