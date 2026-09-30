//! Text access and editing on the [`Editor`](super::Editor): whole-text get and
//! set, programmatic insert/replace, the clipboard commands and the shared
//! text-changing command wrapper.

use crate::buffer::Buffer;
use crate::edit;
use crate::events;
use crate::state::EditorState;
use crate::view::View;

use super::Editor;

impl<M: 'static> Editor<M> {
    /// The whole text.
    pub fn text(&self) -> String {
        self.state.borrow().buffer.text()
    }

    /// Replaces the text, moving the caret to the start and clearing undo.
    pub fn set_text(&self, text: &str) {
        let mut state = self.state.borrow_mut();
        state.buffer = Buffer::new(text);
        state.view = View::new();
        state.reset_highlight();
        drop(state);
        self.control.invalidate();
    }
    /// Inserts `text` at the caret, replacing the selection. Returns whether the
    /// text changed.
    ///
    /// Unlike typing, this does not raise [`Editor::on_change`]; the caller owns
    /// the edit and is responsible for any dirty tracking.
    pub fn insert_text(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            edit::splice(&mut state.buffer, &mut state.view, text, false);
            state.sync_highlight();
        }
        self.control.invalidate();
        true
    }

    /// Replaces the char range `start..end` with `text`. Returns whether the
    /// text changed.
    pub fn replace(&self, start: usize, end: usize, text: &str) -> bool {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let len = state.buffer.len_chars();
            let (start, end) = (start.min(end).min(len), start.max(end).min(len));
            if start == end && text.is_empty() {
                return false;
            }
            state.buffer.replace(start..end, text, false);
            state.view.caret = start + text.chars().count();
            state.view.anchor = state.view.caret;
            state.view.goal_col = None;
            state.sync_highlight();
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
        true
    }

    /// Undoes the last edit, returning whether the text changed.
    ///
    /// The menu Edit → Undo action calls this; it does not raise
    /// [`Editor::on_change`], so the caller owns dirty tracking.
    pub fn undo(&self) -> bool {
        self.edit(|state| edit::undo(&mut state.buffer, &mut state.view))
    }

    /// Redoes the last undone edit, returning whether the text changed.
    pub fn redo(&self) -> bool {
        self.edit(|state| edit::redo(&mut state.buffer, &mut state.view))
    }

    /// Copies the selection (or the caret's line) to the clipboard, returning
    /// whether anything was copied. The text does not change.
    pub fn copy(&self) -> bool {
        let state = self.state.borrow();
        edit::copy(&state.buffer, &state.view, state.clipboard.as_ref())
    }

    /// Cuts the selection (or the caret's line) to the clipboard, returning
    /// whether the text changed.
    pub fn cut(&self) -> bool {
        self.edit(|state| edit::cut(&mut state.buffer, &mut state.view, state.clipboard.as_ref()))
    }

    /// Pastes the clipboard at the caret, replacing the selection. Returns
    /// whether the text changed.
    pub fn paste(&self) -> bool {
        self.edit(|state| edit::paste(&mut state.buffer, &mut state.view, state.clipboard.as_ref()))
    }

    /// Deletes the selection, returning whether the text changed. With no
    /// selection nothing is deleted, matching the Edit → Delete menu action.
    pub fn delete_selection(&self) -> bool {
        self.edit(|state| {
            let Some((start, end)) = state.view.selection() else {
                return false;
            };
            state.buffer.remove(start..end, false);
            state.view.caret = start;
            state.view.anchor = start;
            state.view.goal_col = None;
            true
        })
    }

    /// Selects the whole buffer.
    pub fn select_all(&self) {
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            state.view.select_all(&state.buffer);
        }
        self.control.invalidate();
    }

    /// Runs a text-changing command: re-lexes the affected lines, repaints and
    /// reports whether anything changed.
    fn edit(&self, command: impl FnOnce(&mut EditorState) -> bool) -> bool {
        let ui = self.control.ui().clone();
        let changed = {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let changed = command(state);
            if changed {
                state.sync_highlight();
                // Keep the caret on screen, as the keyboard path does after the
                // same edits (a paste or an undo can move it far away).
                events::ensure_visible(state, &ui, self.control.id());
            }
            changed
        };
        if changed {
            self.control.invalidate();
        }
        changed
    }
}
