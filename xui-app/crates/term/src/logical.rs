//! Logical lines as text with positions: the rows joined by `wrapped`, as a
//! list of characters each remembering the cell it came from. Search and link
//! detection work on these, so a match or a URL may continue across rows.
//!
//! Everything is bounded: a logical line of a million wrapped rows (a program
//! that printed no newline) is read only up to [`MAX_ROWS`] rows and
//! [`MAX_CHARS`] characters.

use crate::Term;

/// The most rows of one logical line read.
pub const MAX_ROWS: u64 = 256;
/// The most characters of one logical line kept.
pub const MAX_CHARS: usize = 64 * 1024;

/// One character and the cell it sits in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
    pub ch: char,
    pub line: u64,
    pub col: usize,
    /// 1, or 2 for a wide character (it covers `col` and `col + 1`).
    pub width: u8,
    /// The hyperlink the cell belongs to (`Attr::link`, 0 for none).
    pub link: u16,
}

/// A logical line: the absolute numbers of its first and last rows, and its
/// characters (blanks included, the last row's trailing blanks trimmed).
pub struct Logical {
    pub first: u64,
    pub last: u64,
    pub chars: Vec<Place>,
}

/// The first row of the logical line `line` belongs to.
pub fn start_of(term: &Term, line: u64) -> u64 {
    let mut start = line;
    while start > term.first_line() && line - start < MAX_ROWS {
        match term.line(start - 1) {
            Some(prev) if prev.wrapped => start -= 1,
            _ => break,
        }
    }
    start
}

/// The logical line that begins at row `first` (which must be a logical
/// line's first row), or `None` past the last row.
pub fn read(term: &Term, first: u64) -> Option<Logical> {
    let end = term.screen_top() + term.rows() as u64;
    let mut chars = Vec::new();
    let mut line = first;
    loop {
        let row = term.line(line)?;
        let cols = term.cols();
        let mut col = 0;
        for cell in row.cells {
            if cell.width != 0 && chars.len() < MAX_CHARS {
                chars.push(Place { ch: cell.ch, line, col, width: cell.width, link: cell.attr.link });
            }
            col += 1;
        }
        let more = row.wrapped && line + 1 < end && line - first + 1 < MAX_ROWS;
        if more {
            // A stored (trimmed) row that wrapped ends in blanks.
            for blank in row.cells.len()..cols {
                if chars.len() < MAX_CHARS {
                    chars.push(Place { ch: ' ', line, col: blank, width: 1, link: 0 });
                }
            }
            line += 1;
        } else {
            while chars.last().is_some_and(|p| p.ch == ' ' && p.line == line) {
                chars.pop();
            }
            return Some(Logical { first, last: line, chars });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_join_across_the_wrap() {
        let mut t = Term::new(5, 4);
        t.feed(b"abcdefgh\r\nxy");
        let top = t.screen_top();
        let l = read(&t, top).unwrap();
        let text: String = l.chars.iter().map(|p| p.ch).collect();
        assert_eq!(text, "abcdefgh");
        assert_eq!((l.first, l.last), (top, top + 1));
        assert_eq!((l.chars[5].line, l.chars[5].col), (top + 1, 0));
        let second = read(&t, top + 2).unwrap();
        assert_eq!(second.chars.len(), 2);
        assert_eq!(start_of(&t, top + 1), top);
    }

    #[test]
    fn a_runaway_line_is_bounded() {
        let mut t = Term::new(80, 4);
        t.feed("x".repeat(80 * 400).as_bytes());
        let top = t.first_line();
        let l = read(&t, top).unwrap();
        assert!(l.last - l.first < MAX_ROWS);
        assert!(l.chars.len() <= MAX_CHARS);
    }

    #[test]
    fn wide_characters_keep_their_cells() {
        let mut t = Term::new(10, 2);
        t.feed("a\u{4e2d}b".as_bytes());
        let l = read(&t, t.screen_top()).unwrap();
        let cells: Vec<(char, usize, u8)> = l.chars.iter().map(|p| (p.ch, p.col, p.width)).collect();
        assert_eq!(cells, [('a', 0, 1), ('\u{4e2d}', 1, 2), ('b', 3, 1)]);
    }
}
