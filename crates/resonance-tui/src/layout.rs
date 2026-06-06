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
    pub footer: Rect,
}

/// Split the full frame: status / EQ curve / spectrum on top (full width),
/// then Effects | Bands side by side, then a footer.
pub fn panes(area: Rect) -> Panes {
    let v = Layout::vertical([
        Constraint::Length(1),  // status
        Constraint::Length(12), // EQ curve (taller)
        Constraint::Length(13), // spectrum
        Constraint::Min(8),     // bands + effects row
        Constraint::Length(1),  // footer
    ])
    .split(area);

    // Bands on the left (wide), effects on the right (narrow).
    let bottom =
        Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)]).split(v[3]);

    Panes {
        status: v[0],
        eq: v[1],
        spectrum: v[2],
        bands: bottom[0],
        effects: bottom[1],
        footer: v[4],
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
/// Gain, Q, Enable, and a gain bar that absorbs the remaining width.
/// The Type column widens to fit full names on wide terminals.
pub fn band_columns(row: Rect) -> Rc<[Rect]> {
    let type_w = if band_type_full(row.width) { 11 } else { 5 };
    let cols = [
        Constraint::Length(3),      // #
        Constraint::Length(type_w), // Type
        Constraint::Length(8),      // Freq
        Constraint::Length(7),      // Gain
        Constraint::Length(6),      // Q
        Constraint::Length(2),      // spacer (extra gap before Enable)
        Constraint::Length(7),      // Enable
        Constraint::Min(0),         // gain bar (fills the rest)
    ];
    Layout::horizontal(cols).spacing(1).split(row)
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
