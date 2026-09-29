#![forbid(unsafe_code)]

//! A byte-bounded undo/redo history of whole-bitmap snapshots.
//!
//! A snapshot per action is simple and never loses a pixel; the cap bounds the
//! memory (default 8 MiB), evicting the oldest undo states first and always
//! keeping at least one so a single action is still undoable.

use super::bitmap::Bitmap;

/// The default history byte cap (8 MiB).
pub const DEFAULT_CAP: usize = 8 * 1024 * 1024;

/// Snapshots of past (undo) and undone (redo) bitmap states.
pub struct History {
    undo: Vec<Bitmap>,
    redo: Vec<Bitmap>,
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

    /// Records the state before a new action and clears the redo stack: a new
    /// action is a new branch.
    pub fn record(&mut self, before: Bitmap) {
        self.drop_redo();
        self.bytes += before.byte_len();
        self.undo.push(before);
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

    /// The bytes held by every snapshot.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte cap.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Swaps `current` for the previous state and pushes `current` onto redo.
    pub fn undo(&mut self, current: &Bitmap) -> Option<Bitmap> {
        let previous = self.undo.pop()?;
        self.bytes -= previous.byte_len();
        self.push_redo(current.clone());
        Some(previous)
    }

    /// Swaps `current` for the next state and pushes `current` onto undo.
    pub fn redo(&mut self, current: &Bitmap) -> Option<Bitmap> {
        let next = self.redo.pop()?;
        self.bytes -= next.byte_len();
        self.push_undo(current.clone());
        Some(next)
    }

    /// Empties both stacks.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
    }

    fn push_undo(&mut self, bitmap: Bitmap) {
        self.bytes += bitmap.byte_len();
        self.undo.push(bitmap);
        self.trim();
    }

    fn push_redo(&mut self, bitmap: Bitmap) {
        self.bytes += bitmap.byte_len();
        self.redo.push(bitmap);
        self.trim();
    }

    fn drop_redo(&mut self) {
        for bitmap in self.redo.drain(..) {
            self.bytes -= bitmap.byte_len();
        }
    }

    /// Evicts the oldest undo states while over the cap, keeping at least one.
    fn trim(&mut self) {
        while self.bytes > self.cap && self.undo.len() > 1 {
            let removed = self.undo.remove(0);
            self.bytes -= removed.byte_len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::bitmap::Pixel;

    fn bitmap(seed: u8) -> Bitmap {
        let mut bitmap = Bitmap::white(4, 4);
        bitmap.put(0, 0, [seed, 0, 0, 255] as Pixel);
        bitmap
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut history = History::new(DEFAULT_CAP);
        let first = bitmap(1);
        let second = bitmap(2);
        history.record(first.clone());
        let back = history.undo(&second).unwrap();
        assert_eq!(back, first);
        assert!(history.can_redo());
        let forward = history.redo(&back).unwrap();
        assert_eq!(forward, second);
    }

    #[test]
    fn a_new_action_clears_the_redo_stack() {
        let mut history = History::new(DEFAULT_CAP);
        history.record(bitmap(1));
        let back = history.undo(&bitmap(2)).unwrap();
        assert!(history.can_redo());
        history.record(back);
        assert!(!history.can_redo(), "a new action branches from here");
    }

    #[test]
    fn the_cap_evicts_the_oldest_and_keeps_one() {
        // Each 64x64 snapshot is 16 KiB; a 32 KiB cap keeps at most two.
        let mut history = History::new(32 * 1024);
        for seed in 0..8 {
            history.record(Bitmap::new(64, 64, [seed, 0, 0, 255]));
        }
        assert!(history.bytes() <= history.cap());
        assert!(history.undo_len() >= 1);
        assert_eq!(history.undo_len(), 2);

        // Even a cap smaller than one snapshot keeps the single step.
        let mut tiny = History::new(1);
        tiny.record(bitmap(1));
        assert_eq!(tiny.undo_len(), 1);
        assert!(tiny.bytes() > tiny.cap());
    }

    #[test]
    fn an_empty_history_has_nothing_to_undo() {
        let mut history = History::new(DEFAULT_CAP);
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert!(history.undo(&bitmap(1)).is_none());
        assert!(history.redo(&bitmap(1)).is_none());
    }
}
