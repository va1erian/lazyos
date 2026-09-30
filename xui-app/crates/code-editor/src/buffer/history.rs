//! The undo/redo machinery: explicit and coalescing groups, the backward and
//! forward splice application, and recording a new [`Edit`](super::Edit).

use super::{Buffer, Edit, Group};

impl Buffer {
    /// Starts an explicit undo group: every edit until [`Buffer::end_edit`]
    /// undoes as one action.
    pub fn begin_edit(&mut self) {
        self.pending = Some(Group {
            edits: Vec::new(),
            coalesce: false,
        });
    }

    /// Stops the current typing/deletion run from coalescing with later edits.
    ///
    /// The editor calls this when the caret moves for a reason other than
    /// typing, so a backspace after navigation starts a fresh undo group.
    pub fn break_coalescing(&mut self) {
        if let Some(group) = self.undo.last_mut() {
            group.coalesce = false;
        }
    }

    /// Ends the group started by [`Buffer::begin_edit`].
    pub fn end_edit(&mut self) {
        if let Some(group) = self.pending.take() {
            self.invalidate_index();
            if !group.edits.is_empty() {
                self.redo.clear();
                self.undo.push(group);
            }
        }
    }

    /// Whether every change has been undone.
    pub fn is_clean(&self) -> bool {
        self.undo.is_empty()
    }

    /// Undoes the last group, returning the caret char index it should move to.
    pub fn undo(&mut self) -> Option<usize> {
        let group = self.undo.pop()?;
        for edit in group.edits.iter().rev() {
            self.apply_backward(edit);
        }
        let caret = group.edits.first().map(|edit| edit.anchor);
        self.invalidate_index();
        self.redo.push(group);
        caret
    }

    /// Redoes the last undone group, returning the caret char index.
    pub fn redo(&mut self) -> Option<usize> {
        let group = self.redo.pop()?;
        for edit in group.edits.iter() {
            self.apply_forward(edit);
        }
        let caret = group
            .edits
            .last()
            .map(|edit| edit.anchor + edit.after_len());
        self.invalidate_index();
        self.undo.push(group);
        caret
    }

    /// Applies `edit` forward.
    fn apply_forward(&mut self, edit: &Edit) {
        let start = edit.anchor.min(self.rope.len_chars());
        let end = (start + edit.before_len()).min(self.rope.len_chars());
        self.note_change(start, end - start, edit.after_len());
        self.rope.remove(start..end);
        self.rope.insert(start, &edit.after);
    }

    /// Applies `edit` backward.
    fn apply_backward(&mut self, edit: &Edit) {
        let start = edit.anchor.min(self.rope.len_chars());
        let end = (start + edit.after_len()).min(self.rope.len_chars());
        self.note_change(start, end - start, edit.before_len());
        self.rope.remove(start..end);
        self.rope.insert(start, &edit.before);
    }

    /// Records `edit`, either into the pending explicit group, into the last
    /// coalescing group, or as a new group.
    pub(super) fn record(&mut self, edit: Edit, coalesce: bool) {
        // The rope changed either way, so the line index is stale even while an
        // explicit group is open and its edits are not yet on the undo stack.
        self.invalidate_index();
        if let Some(group) = self.pending.as_mut() {
            group.edits.push(edit);
            return;
        }
        self.redo.clear();
        if coalesce
            && let Some(group) = self.undo.last_mut()
            && group.coalesce
            && let Some(last) = group.edits.last_mut()
            && last.try_merge(&edit)
        {
            return;
        }
        self.undo.push(Group {
            edits: vec![edit],
            coalesce,
        });
    }
}
