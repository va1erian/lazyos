//! `lazyterm`: the Terminal's portable engine (docs/terminal-plan.md).
//!
//! A [`Term`] is a virtual terminal: bytes the child wrote to its pty go in
//! through [`Term::feed`], a grid of attributed [`Cell`]s, a cursor and the
//! terminal modes come out, and the few things a terminal must answer (cursor
//! position, device attributes) come back as bytes in [`Term::take_reply`] for
//! the host to write to the pty master. Nothing here knows about windows, the
//! pty or LazyOS, so it is host-tested and fuzzed (`fuzz::run`).
//!
//! The parser is the `vte` crate's state machine; the screen semantics on top
//! (margins, scroll regions, wide characters, the alternate screen, deferred
//! wrap) are ours.

mod cell;
mod edit;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod handler;
mod history;
mod row;
mod screen;
mod state;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_history;

pub use cell::{Attr, Cell, Color, Flags};
pub use history::DEFAULT_LINES as DEFAULT_SCROLLBACK;
pub use row::Row;

pub use state::Line;
use state::State;

/// The largest size the engine accepts; a request beyond it is clamped.
pub const MAX_COLS: usize = 1000;
pub const MAX_ROWS: usize = 1000;

fn clamp_size(cols: usize, rows: usize) -> (usize, usize) {
    (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS))
}

/// One line of the scrollback or the screen, as the painter reads it.
/// History lines are stored without their trailing blanks, so `cells` can be
/// shorter than the screen is wide: what is missing is blank.
#[derive(Clone, Copy)]
pub struct LineRef<'a> {
    pub cells: &'a [Cell],
    /// The text continued on the next line by autowrap.
    pub wrapped: bool,
}

/// A virtual terminal screen driven by a program's output.
pub struct Term {
    parser: vte::Parser,
    state: State,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let (cols, rows) = clamp_size(cols, rows);
        Term {
            parser: vte::Parser::new(),
            state: State::new(cols, rows),
        }
    }

    pub fn cols(&self) -> usize {
        self.state.cols()
    }

    pub fn rows(&self) -> usize {
        self.state.height()
    }

    /// Change the size. Rows and columns are cropped or padded (reflow is a
    /// later phase); the cursor stays on screen.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = clamp_size(cols, rows);
        self.state.resize(cols, rows);
    }

    /// Feed the child's output. Returns the logical lines completed by a line
    /// feed, in order: the row text, with the wrapped rows above it joined on,
    /// right-trimmed; empty lines are left out. Those are the host's
    /// evidence lines, independent of the window width.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Line> {
        self.parser.advance(&mut self.state, bytes);
        std::mem::take(&mut self.state.completed)
    }

    /// The bytes to write back to the pty (answers to `CSI 5 n`, `CSI 6 n`,
    /// `CSI c`), emptying the queue.
    pub fn take_reply(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.state.reply)
    }

    /// Row `index` of the visible screen.
    ///
    /// # Panics
    /// If `index >= rows()`.
    pub fn row(&self, index: usize) -> &Row {
        self.state.row_ref(index)
    }

    /// The characters of row `index`, right-trimmed, wide-character spacers
    /// left out.
    pub fn row_text(&self, index: usize) -> String {
        self.state.row_text(index)
    }

    /// The logical line whose last row is `index`.
    pub fn logical_line(&self, index: usize) -> String {
        self.state.logical_line(index)
    }

    /// The cursor as (row, column). A cursor past the last column (deferred
    /// wrap) is reported on the last column.
    pub fn cursor(&self) -> (usize, usize) {
        (self.state.row, self.state.col)
    }

    /// DECTCEM: whether the program wants the cursor drawn.
    pub fn cursor_visible(&self) -> bool {
        self.state.cursor_visible
    }

    /// Whether the alternate screen (`vi`, `less`) is up.
    pub fn on_alternate_screen(&self) -> bool {
        self.state.on_alternate()
    }

    // ---- scrollback ------------------------------------------------------

    /// Set the scrollback budget in lines (default [`DEFAULT_SCROLLBACK`]);
    /// `0` keeps nothing. A smaller budget drops the oldest lines at once.
    pub fn set_scrollback(&mut self, lines: usize) {
        self.state.history.set_limit(lines);
    }

    /// Lines kept in the scrollback.
    pub fn scrollback_len(&self) -> usize {
        self.state.history.len()
    }

    /// Forget the scrollback (`CSI 3 J`).
    pub fn clear_scrollback(&mut self) {
        self.state.history.clear();
    }

    /// The absolute number of the oldest line still kept. Lines are numbered
    /// from the first one that ever scrolled off; numbers are never reused, so
    /// an anchor stored as one stays valid until its text is evicted.
    pub fn first_line(&self) -> u64 {
        self.state.history.first()
    }

    /// The absolute number of the screen's top row: the scrollback is
    /// `first_line()..screen_top()`, the screen the `rows()` after it. It only
    /// grows, by one for every row that leaves the top of the main screen
    /// (kept or not), which is how a scrolled-back view stays on its text.
    pub fn screen_top(&self) -> u64 {
        self.state.history.end()
    }

    /// The line numbered `line` (scrollback or screen), if it still exists.
    pub fn line(&self, line: u64) -> Option<LineRef<'_>> {
        let top = self.screen_top();
        if line < top {
            let row = self.state.history.get(line)?;
            return Some(LineRef {
                cells: &row.cells,
                wrapped: row.wrapped,
            });
        }
        let row = self.state.screen.rows.get(usize::try_from(line - top).ok()?)?;
        Some(LineRef {
            cells: &row.cells,
            wrapped: row.wrapped,
        })
    }
}
