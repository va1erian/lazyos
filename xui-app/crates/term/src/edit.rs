//! Cursor-relative erase, insert and delete: ED, EL, ECH, ICH, DCH, IL, DL.
//! Every erase leaves the current background colour behind.

use crate::state::State;

impl State {
    /// ED: `0` cursor to the end of the screen, `1` start to the cursor,
    /// `2` everything. (The cursor stays where it is, as in xterm.)
    pub fn erase_display(&mut self, mode: usize) {
        let cols = self.cols();
        self.wrap_pending = false;
        let (row, col) = (self.row, self.col);
        match mode {
            0 => {
                self.erase_line_span(row, col, cols);
                for below in row + 1..self.height() {
                    self.erase_line_span(below, 0, cols);
                }
            }
            1 => {
                for above in 0..row {
                    self.erase_line_span(above, 0, cols);
                }
                self.erase_line_span(row, 0, col + 1);
            }
            2 => {
                for all in 0..self.height() {
                    self.erase_line_span(all, 0, cols);
                }
            }
            _ => {}
        }
    }

    /// EL: `0` cursor to the end of the line, `1` start to the cursor, `2`
    /// the whole line.
    pub fn erase_line(&mut self, mode: usize) {
        let cols = self.cols();
        self.wrap_pending = false;
        let (row, col) = (self.row, self.col);
        match mode {
            0 => self.erase_line_span(row, col, cols),
            1 => self.erase_line_span(row, 0, col + 1),
            2 => self.erase_line_span(row, 0, cols),
            _ => {}
        }
    }

    /// Blank `from..to` of `row`. Reaching the end of the row also ends its
    /// wrap: what follows is no longer a continuation of it.
    fn erase_line_span(&mut self, row: usize, from: usize, to: usize) {
        let cols = self.cols();
        let line = &mut self.screen.rows[row];
        line.erase(from, to, self.attr);
        if to >= cols {
            line.wrapped = false;
        }
    }

    /// ECH: blank `n` cells from the cursor without moving anything.
    pub fn erase_chars(&mut self, n: usize) {
        self.wrap_pending = false;
        let (row, col) = (self.row, self.col);
        let line = &mut self.screen.rows[row];
        line.erase(col, col.saturating_add(n), self.attr);
    }

    /// ICH.
    pub fn insert_chars(&mut self, n: usize) {
        self.wrap_pending = false;
        self.screen.insert_cells(self.row, self.col, n, self.attr);
    }

    /// DCH.
    pub fn delete_chars(&mut self, n: usize) {
        self.wrap_pending = false;
        self.screen.delete_cells(self.row, self.col, n, self.attr);
    }

    /// IL: open `n` blank rows at the cursor inside the scroll region.
    pub fn insert_lines(&mut self, n: usize) {
        if self.row < self.top || self.row > self.bottom {
            return;
        }
        self.screen
            .scroll_down(self.row, self.bottom, n, self.attr);
        self.move_to(self.row, 0);
    }

    /// DL: remove `n` rows at the cursor inside the scroll region.
    pub fn delete_lines(&mut self, n: usize) {
        if self.row < self.top || self.row > self.bottom {
            return;
        }
        self.screen.scroll_up(self.row, self.bottom, n, self.attr);
        self.move_to(self.row, 0);
    }

    /// DECALN: fill the screen with `E` (the alignment pattern) and home.
    pub fn alignment_pattern(&mut self) {
        let attr = self.attr;
        for row in &mut self.screen.rows {
            row.erase(0, usize::MAX, attr);
            for cell in &mut row.cells {
                cell.ch = 'E';
            }
            row.wrapped = false;
        }
        self.top = 0;
        self.bottom = self.height() - 1;
        self.move_to(0, 0);
    }
}
