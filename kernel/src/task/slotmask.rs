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

    /// Whether `slot` is marked.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // the run-queue check
    pub fn contains(&self, slot: usize) -> bool {
        self.words
            .get(slot / 64)
            .is_some_and(|word| word.load(Ordering::Acquire) & (1 << (slot % 64)) != 0)
    }

    /// The marked slots right now, without clearing them.
    pub fn snapshot(&self) -> TakenSlots {
        let mut seen = [0u64; WORDS];
        for (out, word) in seen.iter_mut().zip(&self.words) {
            *out = word.load(Ordering::Acquire);
        }
        TakenSlots(seen)
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
    /// The slots marked in either set.
    pub fn union(mut self, other: &TakenSlots) -> TakenSlots {
        for (word, more) in self.0.iter_mut().zip(other.0) {
            *word |= more;
        }
        self
    }

    /// Whether nothing was marked.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }

    /// The marked slots in ascending order. Visits set bits only (one
    /// `trailing_zeros` per slot), so a sparse mask costs its population,
    /// not [`MAX_TASKS`]: the scheduler walks its run queues this way.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(index, &word)| {
            let mut rest = word;
            core::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let bit = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(index * 64 + bit)
            })
        })
    }
}
