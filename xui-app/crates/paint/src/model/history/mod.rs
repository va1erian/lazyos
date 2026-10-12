#![forbid(unsafe_code)]

//! A byte-capped undo/redo history of deltas.
//!
//! Each [`Edit`] holds only the rectangle of one layer it touched (or a
//! structural command), and applying it swaps it with the document, so redo
//! needs no second copy. The cap bounds the memory (default 64 MiB), evicting
//! the oldest undo entries first and always keeping at least one.

mod capture;
mod edit;

pub use capture::UndoCapture;
pub use edit::{Edit, LayerCommand};
pub(crate) use edit::follow;

use super::damage::Damage;
use super::image::Document;

/// The default history byte cap (64 MiB).
pub const DEFAULT_CAP: usize = 64 * 1024 * 1024;

/// Past (undo) and undone (redo) edits.
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    bytes: usize,
    cap: usize,
}

impl History {
    /// An empty history capped at `cap` bytes.
    pub fn new(cap: usize) -> History {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            bytes: 0,
            cap: cap.max(1),
        }
    }

    /// Records a finished edit and clears the redo stack: a new action is a
    /// new branch.
    pub fn record(&mut self, edit: Edit) {
        self.drop_redo();
        self.bytes += edit.bytes();
        self.undo.push(edit);
        self.trim();
    }

    /// Whether an undo is available.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether a redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The number of undo steps.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// The number of redo steps.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// The bytes held by every entry.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte cap.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// The name of the edit Undo would reverse ("Undo Paintbrush").
    pub fn undo_name(&self) -> Option<&str> {
        self.undo.last().map(Edit::name)
    }

    /// The name of the edit Redo would reapply.
    pub fn redo_name(&self) -> Option<&str> {
        self.redo.last().map(Edit::name)
    }

    /// The undo entries' names, oldest first (the History panel).
    pub fn undo_names(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(Edit::name)
    }

    /// The redo entries' names, the next redo first.
    pub fn redo_names(&self) -> impl Iterator<Item = &str> {
        self.redo.iter().rev().map(Edit::name)
    }

    /// Reverses the last edit on `doc`; returns what changed.
    pub fn undo(&mut self, doc: &mut Document) -> Option<Damage> {
        let mut edit = self.undo.pop()?;
        self.bytes -= edit.bytes();
        let damage = edit.apply(doc);
        self.bytes += edit.bytes();
        self.redo.push(edit);
        self.trim();
        Some(damage)
    }

    /// Reapplies the last undone edit on `doc`; returns what changed.
    pub fn redo(&mut self, doc: &mut Document) -> Option<Damage> {
        let mut edit = self.redo.pop()?;
        self.bytes -= edit.bytes();
        let damage = edit.apply(doc);
        self.bytes += edit.bytes();
        self.undo.push(edit);
        self.trim();
        Some(damage)
    }

    /// Empties both stacks.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }

    fn drop_redo(&mut self) {
        for edit in self.redo.drain(..) {
            self.bytes -= edit.bytes();
        }
    }

    /// Evicts the oldest undo entries while over the cap, keeping at least one.
    fn trim(&mut self) {
        let mut evict = 0;
        while self.bytes > self.cap && self.undo.len() - evict > 1 {
            self.bytes -= self.undo[evict].bytes();
            evict += 1;
        }
        self.undo.drain(..evict);
    }
}

#[cfg(test)]
mod tests;
