#![forbid(unsafe_code)]

//! What "unsaved changes" means: where the history was when the file was
//! written, not how often the model's revision moved.
//!
//! The model's revision also moves for things that are not edits (a click on a
//! layer row, a Layer Properties preview that is then cancelled), so the title
//! star and the Save prompt follow the history instead. Every history entry
//! gets an id here, kept in step with the model's stacks after each message, and
//! the file's state is the id on top of the undo stack when it was saved. Undo
//! back to that entry is clean again, as is a selection change followed by the
//! change that restores the saved selection (Paint.NET records selections as
//! history steps, so a selection alone does count as a change).

use crate::model::{History, Selection, Tool};
use crate::view::{Command, Msg};

/// One history entry as the tracker knows it.
#[derive(Clone, Copy)]
struct Entry {
    id: u64,
    /// Whether the step only changed the selection.
    selection_only: bool,
}

/// The saved state against the live history.
#[derive(Default)]
pub(super) struct Saved {
    /// The undo stack, oldest first.
    undo: Vec<Entry>,
    /// The redo stack, the next redo last (the model's own order).
    redo: Vec<Entry>,
    /// The bytes the model's history held when last observed.
    bytes: usize,
    /// The history's eviction count when last observed.
    evicted: u64,
    /// The next id to hand out; 0 is the empty history.
    next: u64,
    /// The id on top of the undo stack in the saved state; `None` when the
    /// picture has no counterpart on disk.
    mark: Option<u64>,
    /// The selection the saved state had.
    selection: Option<Selection>,
}

impl Saved {
    fn fresh(&mut self, selection_only: bool) -> Entry {
        self.next += 1;
        Entry {
            id: self.next,
            selection_only,
        }
    }

    /// The id of the state the history is in now.
    fn position(&self) -> u64 {
        self.undo.last().map_or(0, |entry| entry.id)
    }

    /// Brings the tracker up to date with `history` after one message.
    /// `stepped` says the message was Undo, Redo or a History jump, which move
    /// entries between the stacks instead of adding any; `selection_only` says
    /// a new entry the message made changed only the selection.
    pub(super) fn observe(&mut self, history: &History, stepped: bool, selection_only: bool) {
        let (undo, redo) = (history.undo_len(), history.redo_len());
        let bytes = history.bytes();
        // Entries the cap (or a clear) dropped from the bottom go first, so
        // the ones left line up with the model's again. A recording at the
        // cap keeps the length but swaps the bottom entry for a new top one.
        let evicted = history.evicted().saturating_sub(self.evicted);
        self.evicted = history.evicted();
        let gone = usize::try_from(evicted).unwrap_or(usize::MAX).min(self.undo.len());
        self.undo.drain(..gone);
        let same = undo == self.undo.len() && redo == self.redo.len();
        if same && (stepped || bytes == self.bytes) {
            self.bytes = bytes;
            return;
        }
        if stepped && undo + redo == self.undo.len() + self.redo.len() {
            // Everything stays, only the line between the stacks moves.
            let mut all = std::mem::take(&mut self.undo);
            all.extend(self.redo.drain(..).rev());
            self.redo = all.split_off(undo);
            self.redo.reverse();
            self.undo = all;
        } else {
            self.rebuild(undo, redo, selection_only);
        }
        self.bytes = bytes;
    }

    /// A change that is not a step: new entries on top, the redo stack
    /// dropped or rebuilt.
    fn rebuild(&mut self, undo: usize, redo: usize, selection_only: bool) {
        // Entries below the new ones are the ones that were there (evictions
        // were already taken off the bottom in `observe`).
        self.undo.truncate(undo);
        while self.undo.len() < undo {
            let entry = self.fresh(selection_only);
            self.undo.push(entry);
        }
        self.redo.truncate(redo);
        while self.redo.len() < redo {
            let entry = self.fresh(false);
            self.redo.push(entry);
        }
    }

    /// Records that the picture is as saved (or opened, or new).
    pub(super) fn mark_clean(&mut self, history: &History, selection: Option<&Selection>) {
        self.observe(history, false, false);
        self.mark = Some(self.position());
        self.selection = selection.cloned();
    }

    /// Records that the picture has no file to be in step with.
    pub(super) fn forget(&mut self) {
        self.mark = None;
    }

    /// Whether the history or the selection differs from the saved state.
    pub(super) fn is_dirty(&self, selection: Option<&Selection>) -> bool {
        let Some(mark) = self.mark else {
            return true;
        };
        if self.position() == mark {
            return false;
        }
        // Past the saved entry with nothing but selection steps, and the
        // selection back where it was: no change to the picture.
        let after = if mark == 0 {
            Some(&self.undo[..])
        } else {
            self.undo
                .iter()
                .position(|entry| entry.id == mark)
                .map(|at| &self.undo[at + 1..])
        };
        match after {
            Some(entries) => {
                !entries.iter().all(|entry| entry.selection_only)
                    || self.selection.as_ref() != selection
            }
            None => true,
        }
    }
}

/// Whether a new history entry that `msg` made (with `tool` active) changed
/// only the selection.
pub(super) fn selection_step(msg: &Msg, tool: Tool) -> bool {
    let selecting = matches!(
        tool,
        Tool::RectSelect
            | Tool::EllipseSelect
            | Tool::Lasso
            | Tool::MagicWand
            | Tool::MoveSelection
    );
    match msg {
        Msg::Command(Command::SelectAll | Command::Deselect | Command::InvertSelection) => true,
        Msg::Canvas(_) | Msg::Nudge(..) => selecting,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bitmap, Edit, Rect, WHITE};

    /// A 16 KiB full-layer step on a 64x64 picture.
    fn step() -> Edit {
        Edit::Pixels {
            name: "Test".into(),
            layer: 0,
            rect: Rect::new(0, 0, 64, 64),
            before: Bitmap::new(64, 64, WHITE),
        }
    }

    #[test]
    fn a_step_recorded_at_the_cap_is_a_change() {
        // The cap keeps two steps, so the third evicts the first and the
        // stack keeps its length and its byte count.
        let mut history = History::new(40 * 1024);
        let mut saved = Saved::default();
        history.record(step());
        history.record(step());
        saved.mark_clean(&history, None);
        assert!(!saved.is_dirty(None));

        history.record(step());
        assert_eq!(history.undo_len(), 2);
        saved.observe(&history, false, false);
        assert!(saved.is_dirty(None), "the new step is unsaved");

        history.undo(&mut crate::model::Document::new(64, 64, WHITE).unwrap());
        saved.observe(&history, true, false);
        assert!(!saved.is_dirty(None), "undo is back at the saved step");
    }

    #[test]
    fn a_cleared_history_leaves_nothing_saved_behind() {
        let mut history = History::new(40 * 1024);
        let mut saved = Saved::default();
        history.record(step());
        saved.mark_clean(&history, None);
        history.clear();
        history.record(step());
        saved.observe(&history, false, false);
        assert!(saved.is_dirty(None));
    }
}
