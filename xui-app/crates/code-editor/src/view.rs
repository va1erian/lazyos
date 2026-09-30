#![forbid(unsafe_code)]

//! The editor's caret, selection and scroll state, independent of any backend.
//!
//! Positions are char indices into the [`Buffer`]. Navigation is written here
//! as pure operations so every movement rule (word, line, document, page,
//! shift-selection) is unit-tested without a window.

use crate::buffer::Buffer;
use crate::text::{char_col_for_display, display_col, is_word_char, word_range};

/// The caret, the selection anchor and the scroll position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// The caret char index.
    pub caret: usize,
    /// The selection's fixed end. The selection runs between `anchor` and
    /// `caret`; they are equal when nothing is selected.
    pub anchor: usize,
    /// The first visible line.
    pub first_line: usize,
    /// The first visible display column.
    pub first_col: usize,
    /// The display column vertical movement aims for, so moving through a short
    /// line and back remembers the column.
    pub goal_col: Option<usize>,
    /// Whether a mouse drag is extending the selection.
    pub dragging: bool,
}

impl View {
    /// A view with the caret at the start.
    pub fn new() -> View {
        View::default()
    }

    /// The selected char range, ordered, or `None` when the selection is empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        if self.caret == self.anchor {
            None
        } else if self.caret < self.anchor {
            Some((self.caret, self.anchor))
        } else {
            Some((self.anchor, self.caret))
        }
    }

    /// Moves the caret to `position`, extending the selection when `extend` is
    /// set and collapsing it otherwise.
    fn apply(&mut self, position: usize, extend: bool) {
        self.caret = position;
        if !extend {
            self.anchor = position;
        }
        self.goal_col = None;
    }

    /// Selects everything.
    pub fn select_all(&mut self, buffer: &Buffer) {
        self.anchor = 0;
        self.caret = buffer.len_chars();
        self.goal_col = None;
    }

    /// Clears the selection, keeping the caret.
    pub fn collapse(&mut self) {
        self.anchor = self.caret;
    }

    /// Moves left by one char.
    pub fn left(&mut self, extend: bool) {
        self.apply(self.caret.saturating_sub(1), extend);
    }

    /// Moves right by one char.
    pub fn right(&mut self, buffer: &Buffer, extend: bool) {
        self.apply((self.caret + 1).min(buffer.len_chars()), extend);
    }

    /// Moves left to the previous word boundary.
    pub fn word_left(&mut self, buffer: &Buffer, extend: bool) {
        self.apply(previous_word(buffer, self.caret), extend);
    }

    /// Moves right to the next word boundary.
    pub fn word_right(&mut self, buffer: &Buffer, extend: bool) {
        self.apply(next_word(buffer, self.caret), extend);
    }

    /// Moves to the start of the current line.
    pub fn home(&mut self, buffer: &Buffer, extend: bool) {
        let line = buffer.line_of_char(self.caret);
        self.apply(buffer.line_start(line), extend);
    }

    /// Moves to the end of the current line.
    pub fn end(&mut self, buffer: &Buffer, extend: bool) {
        let line = buffer.line_of_char(self.caret);
        self.apply(buffer.line_end(line), extend);
    }

    /// Moves to the start of the buffer.
    pub fn document_home(&mut self, extend: bool) {
        self.apply(0, extend);
    }

    /// Moves to the end of the buffer.
    pub fn document_end(&mut self, buffer: &Buffer, extend: bool) {
        self.apply(buffer.len_chars(), extend);
    }

    /// Moves up one line, remembering the target display column.
    pub fn up(&mut self, buffer: &Buffer, tab_width: usize, extend: bool) {
        self.vertical(buffer, tab_width, -1, extend);
    }

    /// Moves down one line, remembering the target display column.
    pub fn down(&mut self, buffer: &Buffer, tab_width: usize, extend: bool) {
        self.vertical(buffer, tab_width, 1, extend);
    }

    /// Moves `lines` lines up (negative) or down, for Page Up/Down.
    pub fn page(&mut self, buffer: &Buffer, tab_width: usize, lines: i64, extend: bool) {
        let count = buffer.line_count() as i64;
        let current = buffer.line_of_char(self.caret) as i64;
        let target = (current + lines).clamp(0, count - 1);
        let delta = target - current;
        if delta != 0 {
            self.vertical(buffer, tab_width, delta, extend);
        }
    }

    /// Moves vertically by `delta` lines, preserving the goal column.
    fn vertical(&mut self, buffer: &Buffer, tab_width: usize, delta: i64, extend: bool) {
        let line = buffer.line_of_char(self.caret);
        let goal = self
            .goal_col
            .unwrap_or_else(|| caret_display_col(buffer, tab_width, self.caret));
        let count = buffer.line_count() as i64;
        let target = (line as i64 + delta).clamp(0, count - 1) as usize;
        if target == line && (target == 0 || target as i64 == count - 1) {
            // At the first or last line, a further move goes to the very edge.
            if delta < 0 {
                self.apply(0, extend);
                self.goal_col = Some(goal);
                return;
            }
            if delta > 0 {
                let end = buffer.len_chars();
                self.apply(end, extend);
                self.goal_col = Some(goal);
                return;
            }
        }
        let text = buffer.line_string(target);
        let col = goal.min(display_col(&text, text.chars().count(), tab_width));
        let char_col = char_col_for_display(&text, col, tab_width);
        let position = buffer.line_start(target) + char_col;
        self.apply(position, extend);
        self.goal_col = Some(goal);
    }

    /// Scrolls so the caret is inside a viewport of `visible_lines` lines and
    /// `visible_cols` display columns.
    pub fn ensure_caret_visible(
        &mut self,
        buffer: &Buffer,
        tab_width: usize,
        visible_lines: usize,
        visible_cols: usize,
    ) {
        let line = buffer.line_of_char(self.caret);
        let visible_lines = visible_lines.max(1);
        if line < self.first_line {
            self.first_line = line;
        } else if line >= self.first_line + visible_lines {
            self.first_line = line + 1 - visible_lines;
        }

        let col = caret_display_col(buffer, tab_width, self.caret);
        let visible_cols = visible_cols.max(1);
        if col < self.first_col {
            self.first_col = col;
        } else if col >= self.first_col + visible_cols {
            self.first_col = col + 1 - visible_cols;
        }
    }
}

/// The display column of the caret.
pub fn caret_display_col(buffer: &Buffer, tab_width: usize, caret: usize) -> usize {
    let line = buffer.line_of_char(caret);
    let start = buffer.line_start(line);
    let text = buffer.line_string(line);
    display_col(&text, caret - start, tab_width)
}

/// The char index of the word boundary to the left of `from`.
fn previous_word(buffer: &Buffer, from: usize) -> usize {
    let mut index = from;
    while index > 0 && !buffer.char_at(index - 1).is_some_and(is_word_char) {
        index -= 1;
    }
    while index > 0 && buffer.char_at(index - 1).is_some_and(is_word_char) {
        index -= 1;
    }
    index
}

/// The char index of the word boundary to the right of `from`.
fn next_word(buffer: &Buffer, from: usize) -> usize {
    let len = buffer.len_chars();
    let mut index = from;
    while index < len && buffer.char_at(index).is_some_and(is_word_char) {
        index += 1;
    }
    while index < len && !buffer.char_at(index).is_some_and(is_word_char) {
        index += 1;
    }
    index
}

/// The char range of the word at display column `column` on `line`.
pub fn word_range_at(
    buffer: &Buffer,
    line: usize,
    column: usize,
    tab_width: usize,
) -> (usize, usize) {
    let text = buffer.line_string(line);
    let char_col = char_col_for_display(&text, column, tab_width);
    let (start, end) = word_range(&text, char_col);
    let base = buffer.line_start(line);
    (base + start, base + end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_at(caret: usize) -> View {
        let mut view = View::new();
        view.caret = caret;
        view.anchor = caret;
        view
    }

    #[test]
    fn caret_moves_by_char_and_clamps() {
        let buffer = Buffer::new("abc");
        let mut view = view_at(0);
        view.left(true);
        assert_eq!(view.caret, 0);
        view.right(&buffer, true);
        assert_eq!(view.caret, 1);
        view.document_end(&buffer, true);
        assert_eq!(view.caret, 3);
        view.right(&buffer, true);
        assert_eq!(view.caret, 3);
    }

    #[test]
    fn word_navigation_stops_at_identifier_edges() {
        let buffer = Buffer::new("let total = 12;");
        let mut view = view_at(0);
        view.word_right(&buffer, true);
        assert_eq!(view.caret, 4);
        view.word_right(&buffer, true);
        assert_eq!(view.caret, 12);
        view.word_left(&buffer, true);
        assert_eq!(view.caret, 4);
    }

    #[test]
    fn home_and_end_are_line_local() {
        let buffer = Buffer::new("one\ntwo\n");
        let mut view = view_at(5);
        view.home(&buffer, true);
        assert_eq!(view.caret, 4);
        view.end(&buffer, true);
        assert_eq!(view.caret, 7);
    }

    #[test]
    fn vertical_movement_remembers_the_goal_column() {
        let buffer = Buffer::new("long line\nx\nanother long");
        let mut view = view_at(8);
        view.down(&buffer, 4, true);
        assert_eq!(view.caret, buffer.line_start(1) + 1);
        view.down(&buffer, 4, true);
        assert_eq!(view.caret, buffer.line_start(2) + 8);
    }

    #[test]
    fn page_movement_clamps_to_the_document() {
        let buffer = Buffer::new("0\n1\n2\n3\n4\n5");
        let mut view = view_at(0);
        view.page(&buffer, 4, 3, true);
        assert_eq!(view.caret, buffer.line_start(3));
        view.page(&buffer, 4, 100, true);
        assert_eq!(view.caret, buffer.line_start(5));
        view.page(&buffer, 4, -100, true);
        assert_eq!(view.caret, 0);
    }

    #[test]
    fn selection_is_ordered_and_collapses() {
        let buffer = Buffer::new("hello");
        let mut view = view_at(4);
        view.anchor = 1;
        assert_eq!(view.selection(), Some((1, 4)));
        view.collapse();
        assert_eq!(view.selection(), None);
        assert_eq!(buffer.len_chars(), 5);
    }

    #[test]
    fn ensure_visible_scrolls_both_axes() {
        let buffer = Buffer::new("a\nb\nc\nd\ne");
        let mut view = view_at(buffer.line_start(4));
        view.ensure_caret_visible(&buffer, 4, 2, 80);
        assert_eq!(view.first_line, 3, "the caret line is brought into view");

        let buffer = Buffer::new("0123456789");
        let mut view = view_at(9);
        view.ensure_caret_visible(&buffer, 4, 10, 5);
        assert_eq!(view.first_col, 5);
    }

    #[test]
    fn word_range_at_maps_through_the_line() {
        let buffer = Buffer::new("foo\nbar_baz qux");
        let (start, end) = word_range_at(&buffer, 1, 2, 4);
        let base = buffer.line_start(1);
        assert_eq!((start, end), (base, base + 7));
    }
}
