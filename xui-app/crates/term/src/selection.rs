//! Selecting text: anchors are absolute line numbers and columns, so a
//! selection survives output and scrolling and is dropped only when its text
//! is overwritten or evicted from the scrollback.
//!
//! Four shapes: characters (a drag), words (a double-click, widened by
//! dragging), logical lines (a triple-click, following wrapped rows) and
//! blocks (Alt+drag, a rectangle of cells).

use crate::cell::Cell;
use crate::Term;

/// How a selection grows from its anchor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Char,
    Word,
    Line,
    Block,
}

/// A cell, addressed by absolute line number (see `Term::line`) and column.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Point {
    pub line: u64,
    pub col: usize,
}

impl Point {
    pub fn new(line: u64, col: usize) -> Point {
        Point { line, col }
    }
}

/// The selection a `Term` holds.
#[derive(Clone, Debug)]
pub struct Selection {
    pub(crate) mode: SelectMode,
    pub(crate) anchor: Point,
    pub(crate) head: Point,
    /// The ends of the unit (word or line) the selection began on.
    origin: (Point, Point),
    /// The normalised, inclusive ends (stream modes) or corners (block).
    pub(crate) start: Point,
    pub(crate) end: Point,
}

impl Selection {
    /// Whether anything is selected: a click that has not dragged selects
    /// nothing.
    pub(crate) fn is_active(&self) -> bool {
        match self.mode {
            SelectMode::Char | SelectMode::Block => self.anchor != self.head,
            SelectMode::Word | SelectMode::Line => true,
        }
    }

    /// Columns selected on `line`, inclusive, or `None`.
    pub(crate) fn span(&self, line: u64, cols: usize) -> Option<(usize, usize)> {
        if !self.is_active() || line < self.start.line || line > self.end.line {
            return None;
        }
        let last = cols.saturating_sub(1);
        match self.mode {
            SelectMode::Block => {
                let (a, b) = (self.anchor.col.min(self.head.col), self.anchor.col.max(self.head.col));
                Some((a.min(last), b.min(last)))
            }
            _ => {
                let from = if line == self.start.line { self.start.col } else { 0 };
                let to = if line == self.end.line { self.end.col } else { last };
                Some((from.min(last), to.min(last)))
            }
        }
    }
}

/// The class of a character for word selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Blank,
    Other,
}

impl Term {
    /// The characters besides letters and digits that a double-click keeps in
    /// a word (default `-_./~:@`).
    pub fn set_word_chars(&mut self, chars: &str) {
        self.state.word_chars = chars.to_string();
    }

    fn class(&self, cell: &Cell) -> Class {
        let ch = cell.ch;
        if ch == ' ' || ch == '\0' {
            Class::Blank
        } else if ch.is_alphanumeric() || self.state.word_chars.contains(ch) {
            Class::Word
        } else {
            Class::Other
        }
    }

    /// The cell at (`line`, `col`): blank where a stored line is shorter.
    pub(crate) fn cell_at(&self, line: u64, col: usize) -> Cell {
        self.line(line)
            .and_then(|found| found.cells.get(col).copied())
            .unwrap_or_default()
    }

    fn clamp(&self, point: Point) -> Point {
        let last = self.screen_top() + self.rows() as u64 - 1;
        Point {
            line: point.line.clamp(self.first_line(), last),
            col: point.col.min(self.cols() - 1),
        }
    }

    /// The word (or run of blanks, or punctuation) around `at`.
    fn word_around(&self, at: Point) -> (Point, Point) {
        let mut col = at.col;
        if self.cell_at(at.line, col).width == 0 && col > 0 {
            col -= 1;
        }
        let class = self.class(&self.cell_at(at.line, col));
        let same = |c: usize| {
            let cell = self.cell_at(at.line, c);
            cell.width == 0 || self.class(&cell) == class
        };
        let mut from = col;
        while from > 0 && same(from - 1) {
            from -= 1;
        }
        let mut to = col;
        while to + 1 < self.cols() && same(to + 1) {
            to += 1;
        }
        (Point::new(at.line, from), Point::new(at.line, to))
    }

    /// The logical line around `at`: every row joined by autowrap.
    fn line_around(&self, at: Point) -> (Point, Point) {
        let mut first = at.line;
        while first > self.first_line() && self.line(first - 1).is_some_and(|l| l.wrapped) {
            first -= 1;
        }
        let mut last = at.line;
        while last + 1 < self.screen_top() + self.rows() as u64
            && self.line(last).is_some_and(|l| l.wrapped)
        {
            last += 1;
        }
        (Point::new(first, 0), Point::new(last, self.cols() - 1))
    }

    /// Start a selection at `at`.
    pub fn select_begin(&mut self, mode: SelectMode, at: Point) {
        let at = self.clamp(at);
        let origin = match mode {
            SelectMode::Word => self.word_around(at),
            SelectMode::Line => self.line_around(at),
            _ => (at, at),
        };
        self.state.selection = Some(Selection {
            mode,
            anchor: at,
            head: at,
            origin,
            start: origin.0,
            end: origin.1,
        });
    }

    /// Move the free end to `at` (a drag).
    pub fn select_update(&mut self, at: Point) {
        let at = self.clamp(at);
        let Some(mut sel) = self.state.selection.take() else {
            return;
        };
        sel.head = at;
        let unit = match sel.mode {
            SelectMode::Word => self.word_around(at),
            SelectMode::Line => self.line_around(at),
            SelectMode::Char | SelectMode::Block => (at, at),
        };
        match sel.mode {
            SelectMode::Char | SelectMode::Block => {
                sel.start = sel.anchor.min(at);
                sel.end = sel.anchor.max(at);
            }
            _ if unit.0 < sel.origin.0 => {
                sel.start = unit.0;
                sel.end = sel.origin.1;
            }
            _ => {
                sel.start = sel.origin.0;
                sel.end = unit.1.max(sel.origin.1);
            }
        }
        self.state.selection = Some(sel);
    }

    /// Shift+click: keep the end of the selection farther from `at` and move
    /// the other to it; with no selection, extend from the cursor.
    pub fn select_extend(&mut self, at: Point) {
        let at = self.clamp(at);
        let cols = self.cols() as u128;
        let key = |p: Point| u128::from(p.line) * cols + p.col as u128;
        let existing = self
            .state
            .selection
            .as_ref()
            .filter(|s| s.is_active())
            .map(|s| (s.mode, s.start, s.end));
        let (mode, anchor) = match existing {
            Some((mode, start, end)) => {
                let (to_start, to_end) = (key(at).abs_diff(key(start)), key(at).abs_diff(key(end)));
                (mode, if to_start < to_end { end } else { start })
            }
            None => {
                let cursor = Point::new(self.screen_top() + self.cursor().0 as u64, self.cursor().1);
                (SelectMode::Char, cursor)
            }
        };
        let mode = if mode == SelectMode::Block { mode } else { SelectMode::Char };
        self.select_begin(mode, anchor);
        self.select_update(at);
    }

    /// Select everything, scrollback included.
    pub fn select_all(&mut self) {
        let start = Point::new(self.first_line(), 0);
        let end = Point::new(self.screen_top() + self.rows() as u64 - 1, self.cols() - 1);
        self.select_begin(SelectMode::Char, start);
        self.select_update(end);
    }

    pub fn select_clear(&mut self) {
        self.state.selection = None;
    }

    /// Whether some text is selected.
    pub fn has_selection(&self) -> bool {
        self.state.selection.as_ref().is_some_and(Selection::is_active)
    }

    /// The columns of line `line` that are selected, inclusive.
    pub fn selection_span(&self, line: u64) -> Option<(usize, usize)> {
        self.state.selection.as_ref()?.span(line, self.cols())
    }

    /// The selection's ends (normalised, inclusive), when there is one.
    pub fn selection_range(&self) -> Option<(Point, Point)> {
        let sel = self.state.selection.as_ref().filter(|s| s.is_active())?;
        Some((sel.start, sel.end))
    }

    /// The selected text: wrapped rows joined, hard newlines kept, trailing
    /// blanks trimmed per line, wide-character spacers dropped. A block
    /// selection is one line per row of the rectangle.
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.state.selection.as_ref().filter(|s| s.is_active())?;
        let mut text = String::new();
        for line in sel.start.line..=sel.end.line {
            let (from, to) = sel.span(line, self.cols())?;
            let piece = self.extract(line, from, to);
            let wrapped = self.line(line).is_some_and(|l| l.wrapped);
            let joined = sel.mode != SelectMode::Block && wrapped && line != sel.end.line;
            text.push_str(if joined { &piece } else { piece.trim_end_matches(' ') });
            if line != sel.end.line && !joined {
                text.push('\n');
            }
        }
        // Blank lines at the end (a select-all over an unused screen) are not text.
        text.truncate(text.trim_end_matches('\n').len());
        Some(text)
    }

    /// The characters of `from..=to` on `line`, whole wide characters only.
    fn extract(&self, line: u64, mut from: usize, mut to: usize) -> String {
        if from > 0 && self.cell_at(line, from).width == 0 {
            from -= 1;
        }
        if self.cell_at(line, to).width == 2 {
            to += 1;
        }
        (from..=to.min(self.cols() - 1))
            .map(|col| self.cell_at(line, col))
            .filter(Cell::is_glyph)
            .map(|cell| cell.ch)
            .collect()
    }

    /// Drop the selection if its text was overwritten (`dirty` rows of the
    /// screen) or has left the scrollback. Called after every feed.
    pub(crate) fn validate_selection(&mut self) {
        let top = self.screen_top();
        let Some(sel) = self.state.selection.as_ref() else {
            return;
        };
        let gone = sel.start.line < self.first_line()
            || sel.end.line >= top + self.rows() as u64
            || self.state.dirty.iter().enumerate().any(|(row, &dirty)| {
                let line = top + row as u64;
                dirty && line >= sel.start.line && line <= sel.end.line
            });
        if gone {
            self.state.selection = None;
        }
    }
}
