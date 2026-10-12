//! The visible rows of one screen (the main one or the alternate one) and the
//! row-level edits that do not depend on the cursor: scrolling a region,
//! inserting and deleting cells, resizing.

use crate::cell::{Attr, Cell};
use crate::row::Row;

pub struct Screen {
    pub rows: Vec<Row>,
    cols: usize,
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Screen {
        Screen {
            rows: (0..rows).map(|_| Row::new(cols, Attr::default())).collect(),
            cols,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    /// Scroll rows `top..=bottom` up by `n`: the first `n` leave (returned,
    /// oldest first) and blank rows enter at the bottom.
    pub fn scroll_up(&mut self, top: usize, bottom: usize, n: usize, attr: Attr) -> Vec<Row> {
        let n = n.min(bottom + 1 - top);
        let out: Vec<Row> = self.rows.drain(top..top + n).collect();
        for _ in 0..n {
            self.rows
                .insert(bottom + 1 - n, Row::new(self.cols, attr));
        }
        out
    }

    /// Scroll rows `top..=bottom` down by `n`: the last `n` are dropped and
    /// blank rows enter at the top.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, attr: Attr) {
        let n = n.min(bottom + 1 - top);
        self.rows.drain(bottom + 1 - n..=bottom);
        for _ in 0..n {
            self.rows.insert(top, Row::new(self.cols, attr));
        }
    }

    /// ICH: open `n` blank cells at `col`, pushing the rest right.
    pub fn insert_cells(&mut self, row: usize, col: usize, n: usize, attr: Attr) {
        let cells = &mut self.rows[row].cells;
        let n = n.min(cells.len() - col);
        cells.truncate(cells.len() - n);
        cells.splice(col..col, std::iter::repeat_n(Cell::blank(attr), n));
        self.rows[row].repair(attr);
    }

    /// DCH: remove `n` cells at `col`, pulling the rest left.
    pub fn delete_cells(&mut self, row: usize, col: usize, n: usize, attr: Attr) {
        let cells = &mut self.rows[row].cells;
        let n = n.min(cells.len() - col);
        cells.drain(col..col + n);
        cells.extend(std::iter::repeat_n(Cell::blank(attr), n));
        self.rows[row].repair(attr);
    }

    /// Change the size without reflowing: columns are cut or padded, rows
    /// are added at the bottom or removed from the top while the cursor row
    /// (returned, adjusted) would fall off, then from the bottom. Reflow
    /// belongs to T4.
    pub fn resize(&mut self, cols: usize, rows: usize, cursor_row: usize) -> usize {
        let mut cursor_row = cursor_row;
        for row in &mut self.rows {
            row.cells.resize(cols, Cell::default());
            row.repair(Attr::default());
        }
        self.cols = cols;
        while self.rows.len() > rows && cursor_row + 1 < self.rows.len() {
            self.rows.pop();
        }
        while self.rows.len() > rows {
            self.rows.remove(0);
            cursor_row = cursor_row.saturating_sub(1);
        }
        while self.rows.len() < rows {
            self.rows.push(Row::new(cols, Attr::default()));
        }
        cursor_row.min(rows - 1)
    }
}
