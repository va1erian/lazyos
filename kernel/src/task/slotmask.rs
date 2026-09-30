//! A lock-free bitset with one bit per task slot.
//!
//! The scheduler and the device teardown path each keep a "slots to look at
//! later" mask that interrupt-context code sets and task-context code drains.
//! A single `u64` capped the task table at 64; this spreads the bits over as
//! many words as [`MAX_TASKS`] needs, keeping the same lock-free set/take
//! protocol (every operation is one atomic read-modify-write per word, safe
//! from the timer sweep with its own locks held).

use core::sync::atomic::{AtomicU64, Ordering};

use super::MAX_TASKS;

/// Words in a [`SlotMask`].
pub const WORDS: usize = MAX_TASKS.div_ceil(64);

/// Bit per task slot, settable from any context without taking a lock.
pub struct SlotMask {
    words: [AtomicU64; WORDS],
}

impl SlotMask {
    /// An empty mask.
    pub const fn new() -> Self {
        SlotMask {
            words: [const { AtomicU64::new(0) }; WORDS],
        }
    }

    /// Mark `slot`. Out-of-range slots are ignored.
    pub fn set(&self, slot: usize) {
        if let Some(word) = self.words.get(slot / 64) {
            word.fetch_or(1 << (slot % 64), Ordering::AcqRel);
        }
    }

    /// Unmark `slot`. Out-of-range slots are ignored.
    pub fn clear(&self, slot: usize) {
        if let Some(word) = self.words.get(slot / 64) {
            word.fetch_and(!(1 << (slot % 64)), Ordering::AcqRel);
        }
    }

    /// Whether any slot is marked.
    pub fn any(&self) -> bool {
        self.words
            .iter()
            .any(|word| word.load(Ordering::Acquire) != 0)
    }

    /// Atomically take every marked slot (word by word), leaving the mask
    /// empty. A slot marked while this runs is either returned now or stays
    /// for the next call, never lost.
    pub fn take(&self) -> TakenSlots {
        let mut taken = [0u64; WORDS];
        for (out, word) in taken.iter_mut().zip(&self.words) {
            *out = word.swap(0, Ordering::AcqRel);
        }
        TakenSlots(taken)
    }
}

/// The slots [`SlotMask::take`] removed.
pub struct TakenSlots([u64; WORDS]);

impl TakenSlots {
    /// Whether nothing was marked.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }

    /// Number of marked slots.
    pub fn len(&self) -> usize {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
    }

    /// The marked slots in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..MAX_TASKS).filter(|slot| self.0[slot / 64] & (1 << (slot % 64)) != 0)
    }
}
