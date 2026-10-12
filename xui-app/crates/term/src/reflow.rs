//! Reflow: a new width rewraps the logical lines of the scrollback and the
//! main screen. A logical line is a run of rows joined by `wrapped`; it is
//! cut again at the new width, a wide character never split across two rows,
//! and the cursor stays on the character it was on. The alternate screen is
//! never reflowed (the program repaints after `SIGWINCH`); `State::resize`
//! crops or pads it.
//!
//! Absolute line numbers change, so [`Remap`] says where an old line went (a
//! scrolled-back view follows it). A selection is cleared instead: its columns
//! would need the same mapping per cell, and a resize is rare enough.

use crate::cell::{Attr, Cell};
use crate::history::{encode, History};
use crate::row::Row;
use crate::screen::Screen;

/// Where the lines of one logical line were and are, as stream indices (the
/// oldest kept line is 0).
#[derive(Clone, Copy, Debug)]
struct Span {
    old: u64,
    old_rows: u64,
    new: u64,
    new_rows: u64,
}

/// Old absolute line numbers to new ones, after a resize.
#[derive(Clone, Debug, Default)]
pub struct Remap {
    /// The absolute number of stream index 0 (it does not change).
    base: u64,
    spans: Vec<Span>,
    old_cols: usize,
    new_cols: usize,
    /// The new stream's length.
    new_len: u64,
}

impl Remap {
    /// A resize that moved nothing (a crop, or the alternate screen).
    pub fn identity() -> Remap {
        Remap::default()
    }

    /// The new absolute number of the text that was on line `old`: the same
    /// logical line, at the row the same character offset now falls on.
    pub fn line(&self, old: u64) -> u64 {
        if self.spans.is_empty() {
            return old;
        }
        let Some(index) = old.checked_sub(self.base) else {
            return self.base;
        };
        let at = self.spans.partition_point(|span| span.old <= index);
        let Some(span) = at.checked_sub(1).map(|at| self.spans[at]) else {
            return self.base;
        };
        let inside = index - span.old;
        if inside >= span.old_rows {
            // Past the stream's end: the blank rows below the cursor.
            return self.base + self.new_len + (inside - span.old_rows);
        }
        let offset = inside * self.old_cols as u64 / self.new_cols.max(1) as u64;
        self.base + span.new + offset.min(span.new_rows.saturating_sub(1))
    }
}

/// One logical line: its cells (heads only, widths 1 and 2) and where it was.
struct Logical {
    units: Vec<Cell>,
    old: u64,
    old_rows: u64,
    /// The unit the cursor is on, when it is on this line.
    cursor: Option<usize>,
}

/// Append a row's heads to `units`; returns the cursor's index among them
/// when it is on this row.
fn push_heads(
    units: &mut Vec<Cell>,
    cells: &[Cell],
    pad_to: usize,
    wrapped: bool,
    cursor: Option<(usize, bool)>,
) -> Option<usize> {
    let start = units.len();
    units.extend(cells.iter().filter(|cell| cell.width != 0).copied());
    let at = cursor.map(|(col, pending)| {
        let before = cells[..col.min(cells.len())].iter().filter(|c| c.width != 0).count();
        let on_spacer = col > 0
            && cells.get(col).is_some_and(|c| c.width == 0)
            && cells[col - 1].width == 2;
        start
            + match (pending, on_spacer) {
                (true, _) => before + usize::from(cells.get(col).is_some_and(|c| c.width != 0)),
                (false, true) => before - 1,
                (false, false) => before,
            }
    });
    // A stored history row is short: its tail is blank, and when the row
    // wrapped that tail is text.
    if wrapped && cells.len() < pad_to {
        units.extend(std::iter::repeat_n(Cell::default(), pad_to - cells.len()));
    }
    at
}

/// Rewrap `old` (the main screen with its cursor) and `history` to
/// `cols` x `rows`. Returns the new screen, the cursor row and column, and the
/// remap. The cursor's wrap-pending state is cleared.
pub(crate) fn reflow(
    history: &mut History,
    old: Screen,
    cursor: (usize, usize, bool),
    cols: usize,
    rows: usize,
) -> (Screen, usize, usize, Remap) {
    let old_cols = old.cols();
    let base = history.first();
    let kept = history.take_rows();
    let hist_len = kept.len();
    let (crow, ccol, pending) = cursor;
    let cursor_index = hist_len + crow;

    // 1. The logical lines of the whole stream.
    let mut lines: Vec<Logical> = Vec::new();
    let mut open: Option<Logical> = None;
    let stream = kept
        .iter()
        .map(|row| (&row.cells[..], row.wrapped))
        .chain(old.rows.iter().map(|row| (&row.cells[..], row.wrapped)));
    for (index, (cells, wrapped)) in stream.enumerate() {
        let line = open.get_or_insert_with(|| Logical {
            units: Vec::new(),
            old: index as u64,
            old_rows: 0,
            cursor: None,
        });
        let here = (index == cursor_index).then_some((ccol, pending));
        if let Some(at) = push_heads(&mut line.units, cells, old_cols, wrapped, here) {
            line.cursor = Some(at);
        }
        line.old_rows += 1;
        if !wrapped {
            lines.extend(open.take());
        }
    }
    lines.extend(open.take());

    // 2. Trim each line's blank tail, but not under the cursor; drop the blank
    //    lines below the cursor's.
    for line in &mut lines {
        let keep = line.cursor.map_or(0, |at| at + 1);
        while line.units.len() > keep && line.units.last() == Some(&Cell::default()) {
            line.units.pop();
        }
        if let Some(at) = line.cursor {
            if line.units.len() <= at {
                line.units.resize(at + 1, Cell::default());
            }
        }
    }
    let cursor_line = lines.iter().position(|line| line.cursor.is_some());
    while let (Some(last), Some(at)) = (lines.last(), cursor_line) {
        if lines.len() > at + 1 && last.units.is_empty() {
            lines.pop();
        } else {
            break;
        }
    }

    // 3. Rewrap at the new width.
    let mut grid: Vec<Row> = Vec::new();
    let mut spans = Vec::with_capacity(lines.len());
    let mut cursor_at = (0usize, 0usize);
    for line in &lines {
        let start = grid.len();
        let at = wrap_line(&line.units, cols, &mut grid, line.cursor);
        if let Some(at) = at {
            cursor_at = (start + at.0, at.1);
        }
        spans.push(Span {
            old: line.old,
            old_rows: line.old_rows,
            new: start as u64,
            new_rows: (grid.len() - start) as u64,
        });
    }
    let len = grid.len();

    // 4. The screen is the last `rows` rows, or the cursor's if more follow it.
    let top = len.saturating_sub(rows).min(cursor_at.0);
    let mut screen_rows: Vec<Row> = grid.split_off(top);
    screen_rows.truncate(rows);
    screen_rows.resize_with(rows, || Row::new(cols, Attr::default()));
    history.put_rows(base, grid.iter().map(encode).collect());
    let remap = Remap {
        base,
        spans,
        old_cols,
        new_cols: cols,
        new_len: len as u64,
    };
    (Screen::from_rows(screen_rows, cols), cursor_at.0 - top, cursor_at.1, remap)
}

/// Cut `units` into rows of `cols` and append them to `grid`; returns the
/// (row, column) of unit `cursor` relative to the first row appended.
fn wrap_line(units: &[Cell], cols: usize, grid: &mut Vec<Row>, cursor: Option<usize>) -> Option<(usize, usize)> {
    let first = grid.len();
    grid.push(Row::new(cols, Attr::default()));
    let mut col = 0;
    let mut found = None;
    for (index, unit) in units.iter().enumerate() {
        // A wide character in a one-column terminal cannot be drawn.
        let unit = if unit.width == 2 && cols < 2 { Cell::blank(unit.attr) } else { *unit };
        let width = usize::from(unit.width.max(1));
        if col + width > cols {
            // The row is full, or only one column is left for a wide
            // character: pad it (the engine's marker for that) and wrap.
            if col < cols {
                let row = grid.last_mut().expect("a row");
                row.cells[col] = Cell { ch: ' ', width: 0, attr: Attr::default() };
            }
            grid.last_mut().expect("a row").wrapped = true;
            grid.push(Row::new(cols, Attr::default()));
            col = 0;
        }
        if cursor == Some(index) {
            found = Some((grid.len() - 1 - first, col));
        }
        let row = grid.last_mut().expect("a row");
        row.cells[col] = unit;
        if width == 2 {
            row.cells[col + 1] = Cell { ch: ' ', width: 0, attr: unit.attr };
        }
        col += width;
    }
    found
}
