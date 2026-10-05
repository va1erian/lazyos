//! Where desktop icons sit: columns anchored to the screen's right edge,
//! filled top to bottom, the first column rightmost.
//!
//! The icon view lays its tiles out row by row (slot `k` at row
//! `k / columns`, column `k % columns`). The desktop wants Windows-style
//! columns instead, so the first launchers stay at the top right where new
//! windows (placed from the top left) do not cover them, and a desktop with
//! more icons than one column holds grows leftwards. [`Grid`] maps the view's
//! slots to items; a slot past the last item in a short column is empty.

/// The column layout for `items` icons, `rows` to a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub rows: usize,
    pub columns: usize,
    pub items: usize,
}

impl Grid {
    /// The layout for `items` icons in columns of at most `rows` (at least
    /// one row, at least one column).
    pub fn new(items: usize, rows: usize) -> Grid {
        let rows = rows.max(1);
        Grid {
            rows,
            columns: items.div_ceil(rows).max(1),
            items,
        }
    }

    /// The view slots the layout needs (whole rows of `columns`).
    pub fn slots(&self) -> usize {
        if self.items == 0 {
            return 0;
        }
        self.columns * self.rows.min(self.items)
    }

    /// The item in view slot `slot`, if any.
    pub fn item_at(&self, slot: usize) -> Option<usize> {
        let (row, column) = (slot / self.columns, slot % self.columns);
        if row >= self.rows {
            return None;
        }
        let item = (self.columns - 1 - column) * self.rows + row;
        (item < self.items).then_some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_column_is_the_identity() {
        let grid = Grid::new(6, 12);
        assert_eq!((grid.columns, grid.slots()), (1, 6));
        let items: Vec<_> = (0..6).map(|slot| grid.item_at(slot)).collect();
        assert_eq!(items, (0..6).map(Some).collect::<Vec<_>>());
        assert_eq!(grid.item_at(6), None);
    }

    #[test]
    fn more_icons_grow_columns_leftwards() {
        // 5 items, 3 to a column: 2 columns, the first (items 0-2) on the right.
        let grid = Grid::new(5, 3);
        assert_eq!((grid.columns, grid.slots()), (2, 6));
        let items: Vec<_> = (0..6).map(|slot| grid.item_at(slot)).collect();
        assert_eq!(
            items,
            [Some(3), Some(0), Some(4), Some(1), None, Some(2)],
            "slot 4 is the short left column's empty foot"
        );
    }

    #[test]
    fn nothing_and_degenerate_rows_are_safe() {
        assert_eq!(Grid::new(0, 5).slots(), 0);
        let grid = Grid::new(3, 0);
        assert_eq!((grid.rows, grid.columns), (1, 3));
        assert_eq!(grid.item_at(0), Some(2));
        assert_eq!(grid.item_at(2), Some(0));
        assert_eq!(grid.item_at(3), None);
    }
}
