//! Where the tray sits on the bar: between the window entries and the clock,
//! at most [`TRAY_VISIBLE`] cells of [`CELL`] design pixels, then an overflow
//! chevron for the rest (docs/tray-plan.md section 7.1).
//!
//! Geometry is panel-local design pixels like [`crate::taskbar`]; the shell
//! multiplies by its UI scale at the protocol edge, so a 24 dp cell with a
//! 16 dp icon is 48 and 32 screen pixels at 2x.

use super::icon::ICON_DP;
use super::item::Status;
use super::Tray;
use crate::taskbar::{BAR_H, ENTRY_X};
use crate::Rect;

/// A tray cell's side.
pub const CELL: i32 = 24;
/// The cells' top, centred on the bar.
pub const CELL_Y: i32 = (BAR_H - CELL) / 2;
/// Most cells on the bar; the rest go to the overflow panel.
pub const TRAY_VISIBLE: usize = 6;
/// The overflow chevron's width.
pub const CHEVRON_W: i32 = 16;
/// Space between the tray and the window entries to its left.
pub const TRAY_GAP: i32 = 4;

/// One app's cell on the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub app: String,
    pub rect: Rect,
}

/// The tray's place on the bar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The cells, left to right, in display order.
    pub cells: Vec<Cell>,
    /// The overflow chevron, when anything is in the overflow.
    pub chevron: Option<Rect>,
    /// The apps in the overflow panel, in display order.
    pub overflow: Vec<String>,
    /// The width the tray takes left of the clock, gap included (0 when
    /// empty): what [`crate::taskbar::entry_rects`] must keep free.
    pub reserved: i32,
}

/// What a point on the tray is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit<'a> {
    Item(&'a str),
    Chevron,
}

impl Layout {
    /// What panel-local `(x, y)` is over.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit<'_>> {
        if self.chevron.is_some_and(|rect| rect.contains(x, y)) {
            return Some(Hit::Chevron);
        }
        self.cells
            .iter()
            .find(|cell| cell.rect.contains(x, y))
            .map(|cell| Hit::Item(&cell.app))
    }

    /// The cell of `app`, if it is on the bar.
    pub fn cell(&self, app: &str) -> Option<Rect> {
        self.cells
            .iter()
            .find(|cell| cell.app == app)
            .map(|cell| cell.rect)
    }

    /// Whether `app` has a cell on the bar.
    pub fn visible(&self, app: &str) -> bool {
        self.cell(app).is_some()
    }
}

/// Lay `tray` out to the left of the clock, whose left edge is `clock_x`.
///
/// Hidden items always go to the overflow. When more than [`TRAY_VISIBLE`]
/// remain, the bar keeps the first ones that are not `Passive`, then
/// passive ones, and the rest overflow; the bar still shows its cells in
/// display order. A bar too narrow to fit the cells right of the start
/// button moves cells to the overflow rather than overlapping it.
pub fn layout(tray: &Tray, clock_x: i32) -> Layout {
    let entries = tray.entries();
    let shown: Vec<&str> = entries
        .iter()
        .filter(|entry| !tray.is_hidden(&entry.app))
        .map(|entry| entry.app.as_str())
        .collect();
    let mut on_bar: Vec<&str> = if shown.len() <= TRAY_VISIBLE {
        shown.clone()
    } else {
        let passive = |app: &&str| tray.get(app).is_some_and(|e| e.status() == Status::Passive);
        let mut chosen: Vec<&str> = shown.iter().copied().filter(|app| !passive(app)).collect();
        chosen.extend(shown.iter().copied().filter(passive));
        chosen.truncate(TRAY_VISIBLE);
        // Back to display order.
        shown
            .iter()
            .copied()
            .filter(|app| chosen.contains(app))
            .collect()
    };
    let overflowing = |on_bar: &[&str]| entries.len() > on_bar.len();
    let width = |cells: usize, chevron: bool| {
        // `cells` is at most TRAY_VISIBLE, so this cannot overflow.
        cells as i32 * CELL + if chevron { CHEVRON_W } else { 0 }
    };
    while !on_bar.is_empty()
        && clock_x - width(on_bar.len(), overflowing(&on_bar)) - TRAY_GAP < ENTRY_X
    {
        on_bar.pop();
    }
    let chevron = overflowing(&on_bar);
    let total = width(on_bar.len(), chevron);
    let left = clock_x - total;
    let chevron_rect = chevron.then(|| Rect::new(left, CELL_Y, CHEVRON_W, CELL));
    let first = if chevron { left + CHEVRON_W } else { left };
    let cells = on_bar
        .iter()
        .enumerate()
        .map(|(i, app)| Cell {
            app: (*app).to_owned(),
            rect: Rect::new(first + i as i32 * CELL, CELL_Y, CELL, CELL),
        })
        .collect();
    let overflow = entries
        .iter()
        .map(|entry| entry.app.as_str())
        .filter(|app| !on_bar.contains(app))
        .map(str::to_owned)
        .collect();
    Layout {
        cells,
        chevron: chevron_rect,
        overflow,
        reserved: if total > 0 { total + TRAY_GAP } else { 0 },
    }
}

/// The icon's rectangle inside `cell`: [`ICON_DP`] square, centred.
pub fn icon_rect(cell: Rect) -> Rect {
    Rect::new(
        cell.x + (cell.w - ICON_DP) / 2,
        cell.y + (cell.h - ICON_DP) / 2,
        ICON_DP,
        ICON_DP,
    )
}
