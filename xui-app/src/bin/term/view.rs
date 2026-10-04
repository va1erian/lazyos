//! What the Terminal has put on screen, so a drain repaints only the grid
//! rows that changed (every row when the grid scrolled) instead of the whole
//! window, and so the painter draws only the rows inside the damage.

use xui_core::Rect;

use super::grid::{Grid, ROWS};

/// Where the painter last laid the grid out, in pixels relative to the
/// Terminal's node: the update turns changed rows into a rectangle with it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub pad: i32,
    pub line_h: i32,
    /// Grid rows that fit the window.
    pub visible: usize,
}

impl Layout {
    /// The node-relative band of screen slots `first..=last`.
    fn rows(&self, first: usize, last: usize) -> Rect {
        let top = self.pad + first as i32 * self.line_h;
        let bottom = self.pad + (last as i32 + 1) * self.line_h;
        Rect::new(0, top, self.width, bottom.min(self.height))
    }
}

/// The first grid row shown: the top while the grid is not full, then the
/// rows ending at the cursor so the newest line stays visible.
pub fn first_row(cursor_row: usize, visible: usize) -> usize {
    if cursor_row < visible {
        0
    } else {
        (cursor_row + 1 - visible).min(ROWS - visible)
    }
}

/// The screen as last invalidated: the layout, the first row, the visible
/// rows' characters and the cursor's slot and column.
#[derive(Default)]
pub struct Shown {
    layout: Option<Layout>,
    first: usize,
    rows: Vec<Vec<char>>,
    cursor: (usize, usize),
}

impl Shown {
    /// The node-relative rectangle to repaint so the screen shows `grid` laid
    /// out by `layout` (`None` when nothing visible changed), remembering it
    /// as shown. Without a layout yet (nothing painted) the whole node.
    pub fn update(&mut self, grid: &Grid, layout: Option<Layout>) -> Option<Rect> {
        let Some(layout) = layout else {
            return Some(Rect::new(0, 0, i32::MAX / 2, i32::MAX / 2));
        };
        let visible = layout.visible.clamp(1, ROWS);
        let first = first_row(grid.row, visible);
        let rows: Vec<Vec<char>> = grid.cells[first..(first + visible).min(ROWS)].to_vec();
        let cursor = (grid.row.saturating_sub(first), grid.col);
        let whole =
            self.layout != Some(layout) || self.first != first || self.rows.len() != rows.len();
        let mut changed: Option<(usize, usize)> = None;
        let mut mark = |slot: usize| {
            changed = Some(match changed {
                Some((lo, hi)) => (lo.min(slot), hi.max(slot)),
                None => (slot, slot),
            });
        };
        if whole {
            mark(0);
            mark(rows.len().saturating_sub(1));
        } else {
            for (slot, (old, new)) in self.rows.iter().zip(&rows).enumerate() {
                if old != new {
                    mark(slot);
                }
            }
            if self.cursor != cursor {
                mark(self.cursor.0.min(rows.len().saturating_sub(1)));
                mark(cursor.0.min(rows.len().saturating_sub(1)));
            }
        }
        self.layout = Some(layout);
        self.first = first;
        self.rows = rows;
        self.cursor = cursor;
        let (lo, hi) = changed?;
        Some(if whole {
            Rect::new(0, 0, layout.width, layout.height)
        } else {
            layout.rows(lo, hi)
        })
    }
}

/// Whether two rectangles share a pixel.
pub fn overlaps(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// The pixels two rectangles share (empty when they do not overlap).
pub fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: Layout = Layout {
        width: 640,
        height: 400,
        pad: 10,
        line_h: 20,
        visible: 19,
    };

    #[test]
    fn typing_repaints_only_the_cursor_row() {
        let mut grid = Grid::new();
        let mut shown = Shown::default();
        grid.feed(b"/ # ");
        assert!(shown.update(&grid, Some(LAYOUT)).is_some(), "first frame");
        grid.feed(b"e");
        assert_eq!(
            shown.update(&grid, Some(LAYOUT)),
            Some(Rect::new(0, 10, 640, 30))
        );
        assert_eq!(shown.update(&grid, Some(LAYOUT)), None, "nothing changed");
    }

    #[test]
    fn a_new_line_repaints_both_rows_and_a_scroll_everything() {
        let mut grid = Grid::new();
        let mut shown = Shown::default();
        shown.update(&grid, Some(LAYOUT));
        grid.feed(b"out\r\n");
        assert_eq!(
            shown.update(&grid, Some(LAYOUT)),
            Some(Rect::new(0, 10, 640, 50))
        );
        for _ in 0..30 {
            grid.feed(b"line\r\n");
        }
        assert_eq!(
            shown.update(&grid, Some(LAYOUT)),
            Some(Rect::new(0, 0, 640, 400))
        );
    }

    #[test]
    fn a_new_layout_repaints_everything() {
        let grid = Grid::new();
        let mut shown = Shown::default();
        shown.update(&grid, Some(LAYOUT));
        let wider = Layout {
            width: 800,
            ..LAYOUT
        };
        assert_eq!(
            shown.update(&grid, Some(wider)),
            Some(Rect::new(0, 0, 800, 400))
        );
    }

    #[test]
    fn rectangles() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(overlaps(a, Rect::new(9, 9, 20, 20)));
        assert!(!overlaps(a, Rect::new(10, 0, 20, 10)));
        assert_eq!(
            intersect(a, Rect::new(5, -5, 20, 5)),
            Rect::new(5, 0, 10, 5)
        );
    }
}
