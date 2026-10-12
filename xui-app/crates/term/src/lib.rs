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

mod boxarms;
pub mod boxdraw;
mod cell;
mod edit;
pub mod font;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod handler;
mod history;
pub mod input;
pub mod keys;
mod links;
mod osc;
mod osc_meta;
pub mod palette;
pub mod paste;
pub mod render;
mod reflow;
mod row;
mod selection;
pub mod settings;
mod screen;
mod state;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_color;
#[cfg(test)]
mod tests_input;
#[cfg(test)]
mod tests_select;
#[cfg(test)]
mod tests_history;
#[cfg(test)]
mod tests_modes;
#[cfg(test)]
mod tests_terminfo;
#[cfg(test)]
mod tests_reflow;

pub use cell::{Attr, Cell, Color, Flags};
pub use history::DEFAULT_LINES as DEFAULT_SCROLLBACK;
pub use palette::{Palette, Rgb, Scheme};
pub use osc_meta::{Cwd, MAX_TITLE};
pub use reflow::Remap;
pub use row::Row;
pub use selection::{Point, SelectMode};

pub use state::Line;
use state::State;

/// The largest size the engine accepts; a request beyond it is clamped.
/// The OSC buffer, in bytes: bigger than any URI or path kept (`MAX_URI`,
/// `MAX_CWD`), so a longer one arrives longer than its limit and is refused
/// instead of being cut to something that looks valid; and it bounds the memory
/// a program can make an unterminated OSC take.
const OSC_BUF: usize = 4096;

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
    parser: vte::Parser<OSC_BUF>,
    state: State,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let (cols, rows) = clamp_size(cols, rows);
        Term {
            parser: vte::Parser::<OSC_BUF>::new_with_size(),
            state: State::new(cols, rows),
        }
    }

    pub fn cols(&self) -> usize {
        self.state.cols()
    }

    pub fn rows(&self) -> usize {
        self.state.height()
    }

    /// Change the size. The main screen and the scrollback are reflowed (the
    /// logical lines rewrap, the cursor stays on its character, a wide
    /// character is never split); the alternate screen is cropped or padded.
    /// The selection is cleared. The result maps old absolute line numbers to
    /// new ones, for a view that must stay on its text.
    pub fn resize(&mut self, cols: usize, rows: usize) -> Remap {
        let (cols, rows) = clamp_size(cols, rows);
        self.state.resize(cols, rows)
    }

    /// Feed the child's output. Returns the logical lines completed by a line
    /// feed, in order: the row text, with the wrapped rows above it joined on,
    /// right-trimmed; empty lines are left out. Those are the host's
    /// evidence lines, independent of the window width.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Line> {
        self.state.dirty.fill(false);
        self.parser.advance(&mut self.state, bytes);
        self.validate_selection();
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

    /// The mouse reporting level the program asked for (`?1000`/`?1002`/
    /// `?1003`).
    pub fn mouse_mode(&self) -> input::MouseMode {
        self.state.mouse
    }

    /// Whether mouse reports use the SGR encoding (`?1006`).
    pub fn mouse_sgr(&self) -> bool {
        self.state.mouse_sgr
    }

    /// Whether pasted text should be bracketed (`?2004`).
    pub fn bracketed_paste(&self) -> bool {
        self.state.bracketed_paste
    }

    /// The colours in force: the scheme's, as the program has changed them
    /// (OSC 4/10/11/12).
    pub fn palette(&self) -> &Palette {
        &self.state.palette
    }

    /// Use another colour scheme (a settings change, or the desktop going
    /// dark or light); what a program changed is dropped.
    pub fn set_scheme(&mut self, scheme: Scheme) {
        self.state.palette.set_scheme(scheme);
    }

    // ---- scrollback ------------------------------------------------------

    /// Set the scrollback budget in lines (default [`DEFAULT_SCROLLBACK`]);
    /// `0` keeps nothing. A smaller budget drops the oldest lines at once.
    pub fn set_scrollback(&mut self, lines: usize) {
        self.state.history.set_limit(lines);
        self.validate_selection();
    }

    /// Lines kept in the scrollback.
    pub fn scrollback_len(&self) -> usize {
        self.state.history.len()
    }

    /// The title a program set with OSC 0 or 2, cleaned (printable characters,
    /// at most [`MAX_TITLE`]); `None` when none is set.
    pub fn title(&self) -> Option<&str> {
        self.state.title.as_deref()
    }

    /// How many times the title has changed: the host sets the window title
    /// when this moves.
    pub fn title_generation(&self) -> u64 {
        self.state.title_generation
    }

    /// The working directory the shell reported with OSC 7 (`file://host/path`).
    pub fn cwd(&self) -> Option<&Cwd> {
        self.state.cwd.as_ref()
    }

    /// What keys send now: DECCKM and DECKPAM as the program set them.
    pub fn key_modes(&self) -> keys::KeyModes {
        keys::KeyModes {
            app_cursor: self.state.app_cursor,
            app_keypad: self.state.app_keypad,
        }
    }

    /// The program asked to hear about focus changes (`?1004`): send
    /// [`Term::focus_report`] when the window gains or loses it.
    pub fn focus_reporting(&self) -> bool {
        self.state.focus_reporting
    }

    /// The bytes for a focus change: `CSI I` or `CSI O`, when reporting is on.
    pub fn focus_report(&self, focused: bool) -> Option<&'static [u8]> {
        self.state
            .focus_reporting
            .then_some(if focused { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] })
    }

    /// A program is in the middle of a synchronized update (`?2026`): the
    /// host holds its repaint (with a timeout of its own) until it ends.
    pub fn sync_output(&self) -> bool {
        self.state.sync_output
    }

    /// The URI of the hyperlink on the cell at (`line`, `col`), if any.
    pub fn link_at(&self, line: u64, col: usize) -> Option<&str> {
        let cell = self.line(line)?.cells.get(col)?;
        self.state.links.uri(cell.attr.link)
    }

    /// The hyperlinks on a line as (first column, last column, URI), in order.
    pub fn links_on(&self, line: u64) -> Vec<(usize, usize, &str)> {
        let mut out: Vec<(usize, usize, &str)> = Vec::new();
        let Some(found) = self.line(line) else {
            return out;
        };
        for (col, cell) in found.cells.iter().enumerate() {
            let Some(uri) = self.state.links.uri(cell.attr.link) else {
                continue;
            };
            match out.last_mut() {
                Some(last) if last.1 + 1 == col && last.2 == uri => last.1 = col,
                _ => out.push((col, col, uri)),
            }
        }
        out
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
