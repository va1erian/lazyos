//! Turning a line of cells into runs a painter can draw: neighbouring cells
//! with the same attributes become one run (a background rectangle, a text
//! string), and each run says which column it starts at. No drawing here.

use unicode_width::UnicodeWidthChar;

use crate::cell::{Attr, Cell};

/// Cells that share attributes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Run {
    /// The column of the run's first cell.
    pub col: usize,
    /// How many cells it covers (a wide character covers two).
    pub cells: usize,
    /// The characters, one per glyph cell; wide characters appear once.
    pub text: String,
    pub attr: Attr,
}

/// The cells a character covers on screen: 2 for a wide one, else 1.
pub fn glyph_width(c: char) -> usize {
    match c.width() {
        Some(2) => 2,
        _ => 1,
    }
}

impl Run {
    /// The glyphs of the run: (cell offset from the run's start, character,
    /// cells it covers). Positions come from here, never from adding up the
    /// advances of a string, so a wide or fallback glyph cannot shift the
    /// columns after it.
    pub fn glyphs(&self) -> impl Iterator<Item = (usize, char, usize)> + '_ {
        let mut offset = 0;
        self.text.chars().map(move |c| {
            let width = glyph_width(c);
            let at = offset;
            offset += width;
            (at, c, width)
        })
    }

    /// Nothing to draw for the text: only blanks.
    pub fn is_blank(&self) -> bool {
        self.text.chars().all(|c| c == ' ')
    }
}

/// The runs of the first `cols` cells of a line. Cells past the stored ones
/// (a trimmed history line) are blank with the default attributes, and a
/// trailing blank stretch of default attributes yields no run at all.
pub fn runs(cells: &[Cell], cols: usize) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut col = 0;
    while col < cols {
        let cell = cells.get(col).copied().unwrap_or_default();
        // A spacer is covered by the wide character before it.
        let (ch, width, attr) = if cell.width == 0 {
            (' ', 1, cell.attr)
        } else {
            (cell.ch, usize::from(cell.width), cell.attr)
        };
        let width = width.min(cols - col);
        match out.last_mut() {
            Some(run) if run.attr == attr && run.col + run.cells == col => {
                run.text.push(ch);
                run.cells += width;
            }
            _ => out.push(Run {
                col,
                cells: width,
                text: ch.to_string(),
                attr,
            }),
        }
        col += width.max(1);
    }
    // A default-attribute blank tail needs neither a background nor text.
    if let Some(last) = out.last_mut().filter(|run| run.attr == Attr::default()) {
        let kept = last.text.trim_end_matches(' ').len();
        last.cells -= last.text.len() - kept;
        last.text.truncate(kept);
        if last.text.is_empty() {
            out.pop();
        }
    }
    out
}
