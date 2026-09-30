//! The lazily rebuilt line index: the char index where each line starts, the
//! longest line's length, and the line-break stripping `line_content_len` does.

use ropey::Rope;

/// The char index where each line starts, plus the longest line's length.
///
/// Rebuilt lazily: [`Buffer`](super::Buffer) marks it stale after an edit and
/// the next read rebuilds it in one linear pass.
#[derive(Debug, Default)]
pub(super) struct LineIndex {
    pub(super) starts: Vec<usize>,
    pub(super) max_line_chars: usize,
    /// The widest line in display columns, cached with the tab width it was
    /// measured at; cleared on every rebuild.
    pub(super) max_line_cols: Option<(usize, usize)>,
    pub(super) stale: bool,
}

impl LineIndex {
    /// Rebuilds the index from `rope` if it is stale.
    pub(super) fn ensure(&mut self, rope: &Rope) {
        if !self.stale {
            return;
        }
        self.rebuild(rope);
    }

    /// Rebuilds the index from `rope`.
    pub(super) fn rebuild(&mut self, rope: &Rope) {
        self.starts.clear();
        self.max_line_chars = 0;
        self.max_line_cols = None;
        let lines = rope.len_lines().max(1);
        self.starts.reserve(lines);
        for line in 0..lines {
            self.starts.push(rope.line_to_char(line));
            let len = line_content_len(&rope.line(line));
            self.max_line_chars = self.max_line_chars.max(len);
        }
        self.stale = false;
    }

    /// The line holding `char_idx`, found by binary search over the starts.
    pub(super) fn line_of(&self, char_idx: usize) -> usize {
        match self.starts.binary_search(&char_idx) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        }
    }
}

/// The number of chars in a line slice, excluding its terminator.
///
/// Ropey splits lines on every Unicode break (LF, CRLF, lone CR, VT, FF, NEL,
/// LS and PS), so all of them are stripped here; CR is only paired with a
/// following LF.
pub(super) fn line_content_len(slice: &ropey::RopeSlice<'_>) -> usize {
    let len = slice.len_chars();
    if len == 0 {
        return 0;
    }
    match slice.char(len - 1) {
        '\n' if len >= 2 && slice.char(len - 2) == '\r' => len - 2,
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => len - 1,
        _ => len,
    }
}
