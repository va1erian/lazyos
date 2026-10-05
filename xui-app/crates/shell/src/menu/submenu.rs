//! A category's submenu: the panel that flies out to the right of the start
//! menu when the pointer rests on (or clicks) a category row.
//!
//! It lists the category's apps, one [`ROW_H`] row each, in a panel
//! [`SUB_WIDTH`] wide whose top row lines up with the category row. A panel
//! that would run past the taskbar slides up until its bottom edge sits on
//! the taskbar's top edge; one taller than the room above the taskbar keeps
//! only the rows that fit. Geometry here is panel-local except
//! [`Submenu::origin`], which is in screen design pixels.

use super::{Action, Category, Row, PAD, ROW_H, WIDTH};
use crate::taskbar::BAR_H;
use crate::Rect;

/// Submenu panel width.
pub const SUB_WIDTH: i32 = 200;
/// How far the submenu overlaps the start menu's right edge, so the two
/// borders read as one seam.
const OVERLAP: i32 = 2;

/// One open submenu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submenu {
    /// The category's index in the menu ([`Action::Submenu`]).
    pub category: usize,
    /// The category's `lazypkg` spelling, for the serial marker.
    pub id: &'static str,
    rows: Vec<Row>,
    origin: (i32, i32),
}

impl Submenu {
    /// The submenu of `category` (index `index`) for a category row whose
    /// top edge is at screen `row_top`, on a screen `screen_h` tall.
    pub fn new(index: usize, category: &Category, row_top: i32, screen_h: i32) -> Submenu {
        let room = (screen_h - BAR_H - PAD * 2).max(0);
        let fit = usize::try_from(room / ROW_H).unwrap_or(0);
        let rows: Vec<Row> = category.apps.iter().take(fit).cloned().collect();
        let height = rows.len() as i32 * ROW_H + PAD * 2;
        // The first row's top on the category row's top.
        let wanted = row_top - PAD;
        let y = wanted.min(screen_h - BAR_H - height).max(0);
        Submenu {
            category: index,
            id: category.id,
            rows,
            origin: (WIDTH - OVERLAP, y),
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The panel's screen origin.
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// The panel height.
    pub fn height(&self) -> i32 {
        self.rows.len() as i32 * ROW_H + PAD * 2
    }

    /// Row `index`'s panel-local rectangle.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        (index < self.rows.len())
            .then(|| Rect::new(0, PAD + index as i32 * ROW_H, SUB_WIDTH, ROW_H))
    }

    /// The row under panel-local `(x, y)`.
    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if !(0..SUB_WIDTH).contains(&x) || y < PAD {
            return None;
        }
        let index = usize::try_from((y - PAD) / ROW_H).ok()?;
        (index < self.rows.len()).then_some(index)
    }

    /// The app row `index` launches, if it is one.
    pub fn app(&self, index: usize) -> Option<&str> {
        self.rows
            .get(index)
            .filter(|row| row.enabled && row.action == Action::Launch)
            .map(|row| row.app.as_str())
    }

    /// The row launching `app`.
    pub fn find(&self, app: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.app == app)
    }
}
