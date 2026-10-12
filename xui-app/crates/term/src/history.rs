//! Scrollback: the rows that left the top of the main screen, oldest first.
//!
//! Lines have absolute numbers that only grow: line `first()` is the oldest
//! one kept, and the screen's top row is `end()`. A row that is evicted (or
//! cleared, or never kept because the budget is 0) still leaves its number
//! behind, so an anchor held as an absolute line (a selection, a scrolled-back
//! view) keeps meaning the same text until that text is gone.

use std::collections::VecDeque;

use crate::cell::Cell;
use crate::row::Row;

/// The default budget, in lines.
pub const DEFAULT_LINES: usize = 10_000;

/// A kept row: trailing default blanks are not stored (the reader pads), the
/// wrap flag is.
#[derive(Clone, Debug)]
pub struct HistoryRow {
    pub cells: Box<[Cell]>,
    pub wrapped: bool,
}

pub struct History {
    rows: VecDeque<HistoryRow>,
    limit: usize,
    /// The absolute number of `rows[0]`.
    first: u64,
}

impl Default for History {
    fn default() -> History {
        History::new(DEFAULT_LINES)
    }
}

impl History {
    pub fn new(limit: usize) -> History {
        History {
            rows: VecDeque::new(),
            limit,
            first: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// The absolute number of the oldest kept line (the next one to be
    /// numbered when empty).
    pub fn first(&self) -> u64 {
        self.first
    }

    /// One past the newest kept line: the absolute number of the screen's top
    /// row.
    pub fn end(&self) -> u64 {
        self.first + self.rows.len() as u64
    }

    /// Take a row that scrolled off the top.
    pub fn push(&mut self, row: &Row) {
        if self.limit == 0 {
            self.first += 1;
            return;
        }
        let used = row
            .cells
            .iter()
            .rposition(|cell| *cell != Cell::default())
            .map_or(0, |at| at + 1);
        self.rows.push_back(HistoryRow {
            cells: Box::from(&row.cells[..used]),
            wrapped: row.wrapped,
        });
        self.evict();
    }

    /// Forget every line (their numbers are not reused).
    pub fn clear(&mut self) {
        self.first += self.rows.len() as u64;
        self.rows.clear();
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.evict();
    }

    pub fn get(&self, line: u64) -> Option<&HistoryRow> {
        let at = usize::try_from(line.checked_sub(self.first)?).ok()?;
        self.rows.get(at)
    }

    fn evict(&mut self) {
        while self.rows.len() > self.limit {
            self.rows.pop_front();
            self.first += 1;
        }
        if self.rows.capacity() > 2 * self.rows.len() + 1024 {
            self.rows.shrink_to_fit();
        }
    }

    /// Whether the invariants hold (tests and the fuzzer).
    #[cfg(any(test, feature = "fuzz"))]
    pub fn is_consistent(&self) -> bool {
        self.rows.len() <= self.limit
            && self
                .rows
                .iter()
                .all(|row| row.cells.last().is_none_or(|cell| *cell != Cell::default()))
    }
}
