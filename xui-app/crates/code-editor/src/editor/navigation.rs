//! Caret and selection navigation, marker/option configuration and the
//! read-only command-state queries on the [`Editor`](super::Editor).

use crate::events;
use crate::markers::Marker;
use crate::options::Options;

use super::Editor;

impl<M: 'static> Editor<M> {
    /// Moves the caret to `line`/`col` (both zero-based, `col` in chars) and
    /// scrolls it into view.
    pub fn goto(&self, line: usize, col: usize) {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let line = line.min(state.buffer.line_count().saturating_sub(1));
            let start = state.buffer.line_start(line);
            let end = state.buffer.line_end(line);
            let position = start + col.min(end - start);
            state.view.caret = position;
            state.view.anchor = position;
            state.view.goal_col = None;
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
    }

    /// Replaces the diagnostic markers (squiggles, tints and breakpoints).
    pub fn set_markers(&self, markers: Vec<Marker>) {
        self.state.borrow_mut().markers = markers;
        self.control.invalidate();
    }

    /// The caret's char offset.
    pub fn caret(&self) -> usize {
        self.state.borrow().view.caret
    }

    /// The caret as a zero-based `(line, column)` pair, `column` in chars.
    pub fn caret_line_col(&self) -> (usize, usize) {
        let state = self.state.borrow();
        let caret = state.view.caret;
        let line = state.buffer.line_of_char(caret);
        (line, caret - state.buffer.line_start(line))
    }

    /// The buffer's revision counter: it moves on every text change and stays
    /// put otherwise, so a caller can tell whether a command changed the text
    /// without comparing it.
    pub fn revision(&self) -> u64 {
        self.state.borrow().buffer.revision()
    }

    /// Moves the caret to the char offset `offset`, clearing any selection and
    /// scrolling it into view.
    pub fn set_caret(&self, offset: usize) {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let offset = offset.min(state.buffer.len_chars());
            state.view.caret = offset;
            state.view.anchor = offset;
            state.view.goal_col = None;
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
    }

    /// Selects the char range `start..end` (ordered) and scrolls to it.
    pub fn select(&self, start: usize, end: usize) {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let (start, end) = (start.min(end), start.max(end));
            state.view.anchor = start.min(state.buffer.len_chars());
            state.view.caret = end.min(state.buffer.len_chars());
            state.view.goal_col = None;
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
    }
    /// Replaces the display options.
    pub fn set_options(&self, options: Options) {
        self.state.borrow_mut().options = options;
        self.control.invalidate();
    }

    /// Whether Undo has an edit to undo.
    pub fn can_undo(&self) -> bool {
        self.state.borrow().buffer.can_undo()
    }

    /// Whether Redo has an edit to redo.
    pub fn can_redo(&self) -> bool {
        self.state.borrow().buffer.can_redo()
    }

    /// Whether the clipboard holds text a paste would insert.
    pub fn can_paste(&self) -> bool {
        self.state
            .borrow()
            .clipboard
            .text()
            .is_some_and(|text| !text.is_empty())
    }

    /// Whether the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.state.borrow().buffer.len_chars() == 0
    }

    /// The selected char range, ordered, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.state.borrow().view.selection()
    }
}
