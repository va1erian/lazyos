//! A screen row: cells plus whether it continues onto the next row because
//! the text wrapped at the margin (not because of a newline).

use crate::cell::{Attr, Cell};

#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The text went on past the last column onto the next row.
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, attr: Attr) -> Row {
        Row {
            cells: vec![Cell::blank(attr); cols],
            wrapped: false,
        }
    }

    /// Blank `from..to` (clamped) with `attr`'s background, then repair any
    /// wide character the range cut in half.
    pub fn erase(&mut self, from: usize, to: usize, attr: Attr) {
        let to = to.min(self.cells.len());
        if from < to {
            self.cells[from..to].fill(Cell::blank(attr));
            self.repair(attr);
        }
    }

    /// The text of the row, spacers left out. With `trim`, trailing blanks
    /// are dropped.
    pub fn text(&self, trim: bool) -> String {
        let mut text: String = self
            .cells
            .iter()
            .filter(|cell| cell.is_glyph())
            .map(|cell| cell.ch)
            .collect();
        if trim {
            text.truncate(text.trim_end_matches(' ').len());
        }
        text
    }

    /// Restore the wide-character invariant after a bulk edit: a wide head is
    /// followed by its spacer, and a spacer (outside the last column's pad)
    /// follows a head. Orphans become blanks.
    pub fn repair(&mut self, attr: Attr) {
        let last = self.cells.len().saturating_sub(1);
        for i in 0..self.cells.len() {
            let width = self.cells[i].width;
            let headless = i == 0 || self.cells[i - 1].width != 2;
            if (width == 2 && (i == last || self.cells[i + 1].width != 0))
                || (width == 0 && headless && i != last)
            {
                self.cells[i] = Cell::blank(attr);
            }
        }
    }

    /// Blank the other half of the wide character that cell `col` belongs to,
    /// before `col` is overwritten.
    pub fn sever(&mut self, col: usize, attr: Attr) {
        let Some(cell) = self.cells.get(col) else {
            return;
        };
        match cell.width {
            2 if col + 1 < self.cells.len() => self.cells[col + 1] = Cell::blank(attr),
            0 if col > 0 && self.cells[col - 1].width == 2 => {
                self.cells[col - 1] = Cell::blank(attr)
            }
            _ => {}
        }
    }

    /// Whether the wide-character invariant holds (tests and the fuzzer).
    pub fn is_consistent(&self) -> bool {
        let last = self.cells.len().saturating_sub(1);
        self.cells.iter().enumerate().all(|(i, cell)| match cell.width {
            1 => true,
            2 => i < last && self.cells[i + 1].width == 0,
            0 => (i > 0 && self.cells[i - 1].width == 2) || i == last,
            _ => false,
        })
    }
}
