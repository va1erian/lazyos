//! The terminal state a program's output drives: the screen, the cursor, the
//! modes, and the cursor-addressed edits. The escape-sequence layer
//! (`handler.rs`) only decodes bytes into calls on this.

use unicode_width::UnicodeWidthChar;

use crate::cell::{Attr, Cell};
use crate::history::History;
use crate::input::MouseMode;
use crate::links::Links;
use crate::osc_meta::Cwd;
use crate::palette::{scheme_by_name, Palette};
use crate::selection::Selection;
use crate::reflow::{reflow, Remap};
use crate::row::Row;
use crate::screen::Screen;

/// How much of a wrapped line that scrolled off the top is kept for the
/// evidence line (it is only a prefix for `TERM:OUT` matching).
const CARRY_MAX: usize = 64 * 1024;

/// A logical line a line feed completed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    /// The row text with the auto-wrapped rows above it joined on,
    /// right-trimmed.
    pub text: String,
    /// The last row ran up to the final column. BusyBox's line editor breaks a
    /// long command there with its own `CR LF` instead of relying on
    /// autowrap, so such a line may continue on the next one (the host
    /// decides; the engine cannot tell it from a line that is exactly as wide
    /// as the screen).
    pub ends_at_margin: bool,
}

/// What DECSC saves.
#[derive(Clone, Copy)]
struct Saved {
    row: usize,
    col: usize,
    attr: Attr,
    wrap_pending: bool,
}

/// The main screen and its cursor while the alternate screen is up.
struct Stash {
    screen: Screen,
    row: usize,
    col: usize,
    wrap_pending: bool,
}

pub(crate) struct State {
    pub screen: Screen,
    pub history: History,
    pub palette: Palette,
    /// The current selection (`selection.rs`) and the characters besides
    /// alphanumerics a word may hold.
    pub selection: Option<Selection>,
    pub word_chars: String,
    /// Screen rows written since the last feed began: a selection over one
    /// of them is gone.
    pub dirty: Vec<bool>,
    /// Mouse reporting (`?1000`, `?1002`, `?1003`), its SGR encoding (`?1006`)
    /// and bracketed paste (`?2004`), as the program set them.
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub bracketed_paste: bool,
    /// DECCKM, DECKPAM (what keys send), focus reporting (`?1004`) and
    /// synchronized output (`?2026`): see `keys.rs`.
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub focus_reporting: bool,
    pub sync_output: bool,
    /// The window title a program set (OSC 0/2; `None` when it has not, or
    /// cleared it), counted so the host sees a change; the working directory
    /// the shell reported (OSC 7); the hyperlinks cells point at (OSC 8).
    pub title: Option<String>,
    pub title_generation: u64,
    pub cwd: Option<Cwd>,
    pub links: Links,
    stash: Option<Stash>,
    pub row: usize,
    pub col: usize,
    /// The last character filled the final column: the next printable one
    /// wraps (DECAWM's deferred wrap), but a cursor move cancels it.
    pub wrap_pending: bool,
    pub attr: Attr,
    saved: Option<Saved>,
    /// The scroll region, inclusive row indices.
    pub top: usize,
    pub bottom: usize,
    pub tabs: Vec<bool>,
    pub autowrap: bool,
    pub cursor_visible: bool,
    /// A line feed also returns the carriage (LNM, off by default as in
    /// xterm). The tty's ONLCR already sends `\r\n` for cooked output, and a
    /// raw-mode program that clears OPOST expects a bare `\n` to keep the column.
    pub newline: bool,
    /// Bytes to write back to the pty (answers to DSR and DA).
    pub reply: Vec<u8>,
    /// Logical lines completed by a line feed since the last `take`.
    pub completed: Vec<Line>,
    /// The text of wrapped rows that scrolled off the top of the main screen,
    /// the start of the logical line being written.
    carry: String,
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|col| col % 8 == 0).collect()
}

impl State {
    pub fn new(cols: usize, rows: usize) -> State {
        State {
            screen: Screen::new(cols, rows),
            history: History::default(),
            palette: Palette::new(scheme_by_name("desktop", true)),
            selection: None,
            word_chars: String::from("-_./~:@"),
            dirty: vec![false; rows],
            mouse: MouseMode::Off,
            mouse_sgr: false,
            bracketed_paste: false,
            app_cursor: false,
            app_keypad: false,
            focus_reporting: false,
            sync_output: false,
            title: None,
            title_generation: 0,
            cwd: None,
            links: Links::default(),
            stash: None,
            row: 0,
            col: 0,
            wrap_pending: false,
            attr: Attr::default(),
            saved: None,
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            autowrap: true,
            cursor_visible: true,
            newline: false,
            reply: Vec::new(),
            completed: Vec::new(),
            carry: String::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.screen.cols()
    }

    pub fn height(&self) -> usize {
        self.screen.height()
    }

    pub fn on_alternate(&self) -> bool {
        self.stash.is_some()
    }

    // ---- printing -------------------------------------------------------

    pub fn print(&mut self, c: char) {
        // Zero-width (combining) and control characters take no cell here.
        let width = match c.width() {
            Some(width @ (1 | 2)) => width,
            _ => return,
        };
        let cols = self.cols();
        if width > cols {
            return;
        }
        if self.wrap_pending {
            self.wrap_pending = false;
            if self.autowrap {
                self.wrap();
            }
        }
        let mut col = self.col;
        if col + width > cols {
            if self.autowrap {
                // The wide character does not fit: pad the last column.
                self.dirty[self.row] = true;
                let attr = self.attr;
                let row = &mut self.screen.rows[self.row];
                row.sever(cols - 1, attr);
                row.cells[cols - 1] = Cell {
                    ch: ' ',
                    width: 0,
                    attr: attr.erased(),
                };
                self.wrap();
                col = 0;
            } else {
                col = cols - width;
            }
        }
        self.dirty[self.row] = true;
        let (attr, row) = (self.attr, &mut self.screen.rows[self.row]);
        row.sever(col, attr);
        if width == 2 {
            row.sever(col + 1, attr);
        }
        row.cells[col] = Cell {
            ch: c,
            width: width as u8,
            attr,
        };
        if width == 2 {
            row.cells[col + 1] = Cell {
                ch: ' ',
                width: 0,
                attr,
            };
        }
        col += width;
        if col >= cols {
            self.col = cols - 1;
            self.wrap_pending = self.autowrap;
        } else {
            self.col = col;
        }
    }

    /// Continue on the next row because the text hit the margin.
    fn wrap(&mut self) {
        self.screen.rows[self.row].wrapped = true;
        self.col = 0;
        self.index();
    }

    // ---- line feeds and scrolling ----------------------------------------

    /// A line feed: report the logical line being left, then move down.
    pub fn line_feed(&mut self) {
        let text = self.logical_line(self.row);
        if !text.is_empty() {
            let ends_at_margin = self.screen.rows[self.row].cells.last().is_some_and(|cell| {
                cell.width != 1 || cell.ch != ' '
            });
            self.completed.push(Line {
                text,
                ends_at_margin,
            });
        }
        self.carry.clear();
        self.index();
        if self.newline {
            self.col = 0;
        }
    }

    /// IND: down one row, scrolling the region at its bottom margin.
    pub fn index(&mut self) {
        self.wrap_pending = false;
        if self.row == self.bottom {
            self.scroll_up(1);
        } else if self.row + 1 < self.height() {
            self.row += 1;
        }
    }

    /// RI: up one row, scrolling the region down at its top margin.
    pub fn reverse_index(&mut self) {
        self.wrap_pending = false;
        if self.row == self.top {
            self.scroll_down(1);
        } else {
            self.row = self.row.saturating_sub(1);
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        // Only the main screen scrolled from its very top feeds the history.
        let keep = self.top == 0 && !self.on_alternate();
        // Rows keep their numbers when the whole main screen scrolls, so only
        // the row that came in is new; elsewhere the region's text moved.
        if keep {
            self.dirty[self.bottom] = true;
        } else {
            self.dirty[self.top..=self.bottom].fill(true);
        }
        let (carry, history) = (&mut self.carry, &mut self.history);
        self.screen
            .scroll_up(self.top, self.bottom, n, self.attr, |row| {
                if !keep {
                    return;
                }
                if row.wrapped && carry.len() < CARRY_MAX {
                    carry.push_str(&row.text(false));
                } else {
                    carry.clear();
                }
                history.push(row);
            });
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.dirty[self.top..=self.bottom].fill(true);
        self.screen.scroll_down(self.top, self.bottom, n, self.attr);
    }

    /// The logical (unwrapped) line that ends on `row`, right-trimmed:
    /// wrapped rows above it are joined on, so a command that wrapped at the
    /// margin is read whole.
    pub fn logical_line(&self, row: usize) -> String {
        let mut start = row;
        while start > 0 && self.screen.rows[start - 1].wrapped {
            start -= 1;
        }
        let mut text = if start == 0 && !self.on_alternate() {
            self.carry.clone()
        } else {
            String::new()
        };
        for (at, line) in self.screen.rows[start..=row].iter().enumerate() {
            text.push_str(&line.text(start + at == row));
        }
        text
    }

    // ---- cursor ---------------------------------------------------------

    pub fn move_to(&mut self, row: usize, col: usize) {
        self.wrap_pending = false;
        self.row = row.min(self.height() - 1);
        self.col = col.min(self.cols() - 1);
    }

    /// CUU stops at the top margin when the cursor starts inside the region.
    pub fn move_up(&mut self, n: usize) {
        let limit = if self.row >= self.top { self.top } else { 0 };
        self.move_to(self.row.saturating_sub(n).max(limit), self.col);
    }

    /// CUD stops at the bottom margin when the cursor starts inside the region.
    pub fn move_down(&mut self, n: usize) {
        let limit = if self.row <= self.bottom {
            self.bottom
        } else {
            self.height() - 1
        };
        self.move_to(self.row.saturating_add(n).min(limit), self.col);
    }

    pub fn save_cursor(&mut self) {
        self.saved = Some(Saved {
            row: self.row,
            col: self.col,
            attr: self.attr,
            wrap_pending: self.wrap_pending,
        });
    }

    /// DECRC; with nothing saved, home with default attributes.
    pub fn restore_cursor(&mut self) {
        let saved = self.saved.unwrap_or(Saved {
            row: 0,
            col: 0,
            attr: Attr::default(),
            wrap_pending: false,
        });
        self.move_to(saved.row, saved.col);
        self.attr = saved.attr;
        self.wrap_pending = saved.wrap_pending && self.col == self.cols() - 1;
    }

    /// DECSTBM: set the scroll region (ignored unless it spans two rows or
    /// more) and home the cursor.
    pub fn set_region(&mut self, top: usize, bottom: usize) {
        if top < bottom && bottom < self.height() {
            self.top = top;
            self.bottom = bottom;
            self.move_to(0, 0);
        }
    }

    // ---- tab stops ------------------------------------------------------

    /// HT / CHT: forward `n` tab stops (the last column when none is left).
    pub fn tab_forward(&mut self, n: usize) {
        let mut col = self.col;
        for _ in 0..n.min(self.cols()) {
            col = (col + 1..self.cols())
                .find(|&at| self.tabs[at])
                .unwrap_or(self.cols() - 1);
        }
        self.move_to(self.row, col);
    }

    /// CBT: back `n` tab stops (column 0 when none is left).
    pub fn tab_back(&mut self, n: usize) {
        let mut col = self.col;
        for _ in 0..n.min(self.cols()) {
            col = (0..col).rev().find(|&at| self.tabs[at]).unwrap_or(0);
        }
        self.move_to(self.row, col);
    }

    // ---- alternate screen, reset, resize ---------------------------------

    /// Switch to (`on`) or back from the alternate screen: what `vi` and
    /// `less` draw on, so the shell's screen comes back as it was.
    pub fn set_alternate(&mut self, on: bool) {
        self.selection = None;
        if on && self.stash.is_none() {
            let blank = Screen::new(self.cols(), self.height());
            self.stash = Some(Stash {
                screen: std::mem::replace(&mut self.screen, blank),
                row: self.row,
                col: self.col,
                wrap_pending: self.wrap_pending,
            });
        } else if !on {
            if let Some(main) = self.stash.take() {
                self.screen = main.screen;
                self.row = main.row;
                self.col = main.col;
                self.wrap_pending = main.wrap_pending;
            }
        }
        self.top = 0;
        self.bottom = self.height() - 1;
    }

    /// RIS: back to the power-on state (the screen cleared, the main one up).
    pub fn reset(&mut self) {
        let mut fresh = State::new(self.cols(), self.height());
        std::mem::swap(&mut fresh.reply, &mut self.reply);
        std::mem::swap(&mut fresh.completed, &mut self.completed);
        // The history goes, its budget stays, its numbers keep growing.
        std::mem::swap(&mut fresh.history, &mut self.history);
        fresh.word_chars = std::mem::take(&mut self.word_chars);
        std::mem::swap(&mut fresh.palette, &mut self.palette);
        fresh.palette.reset();
        fresh.history.clear();
        // The title goes with the rest, but the host must see that it changed.
        fresh.title_generation = self.title_generation + u64::from(self.title.is_some());
        *self = fresh;
    }

    /// Resize: the main screen and the scrollback are reflowed (`reflow.rs`),
    /// the alternate screen is cropped or padded.
    pub fn resize(&mut self, cols: usize, rows: usize) -> Remap {
        self.selection = None;
        self.carry.clear();
        self.dirty = vec![false; rows];
        let remap;
        if let Some(main) = &mut self.stash {
            self.row = self.screen.resize(cols, rows, self.row);
            self.col = self.col.min(cols - 1);
            self.wrap_pending &= self.col == cols - 1;
            let old = std::mem::replace(&mut main.screen, Screen::new(1, 1));
            let cursor = (main.row, main.col, main.wrap_pending);
            let (screen, row, col, map) = reflow(&mut self.history, old, cursor, cols, rows);
            (main.screen, main.row, main.col, main.wrap_pending) = (screen, row, col, false);
            remap = map;
        } else {
            let old = std::mem::replace(&mut self.screen, Screen::new(1, 1));
            let cursor = (self.row, self.col, self.wrap_pending);
            let (screen, row, col, map) = reflow(&mut self.history, old, cursor, cols, rows);
            (self.screen, self.row, self.col, self.wrap_pending) = (screen, row, col, false);
            remap = map;
        }
        self.top = 0;
        self.bottom = rows - 1;
        self.tabs.truncate(cols);
        let have = self.tabs.len();
        self.tabs.extend((have..cols).map(|col| col % 8 == 0));
        if let Some(saved) = &mut self.saved {
            saved.row = saved.row.min(rows - 1);
            saved.col = saved.col.min(cols - 1);
        }
        remap
    }

    /// A row's text, right-trimmed.
    pub fn row_text(&self, row: usize) -> String {
        self.screen.rows[row].text(true)
    }

    pub fn row_ref(&self, row: usize) -> &Row {
        &self.screen.rows[row]
    }
}
