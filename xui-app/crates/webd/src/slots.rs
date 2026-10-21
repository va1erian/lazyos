//! The slot state machine of one view (docs/webd-plan.md 3.4, `os.lazy.webd.light.v1`).
//!
//! The client owns the pixels: it attaches up to [`SLOTS_PER_VIEW`] buffers and a slot is *free*
//! (webd may paint into it) or *busy* (webd handed it out in a `Frame`; the client's until
//! `ReleaseSlot`). A frame goes to the lowest free slot; when there is none the frame is dropped,
//! never queued, and the caller asks the engine for a fresh one once a slot is released. A busy
//! slot is never painted into, replaced or taken back, except by [`Slots::detach_all`] when the view
//! changes size and every slot is the wrong size at once.
//!
//! Pure: the memory `M` is whatever the caller keeps per slot (a mapping in the service, a number
//! in the fuzz test), so the rules are tested without a fabric.

use crate::limits::SLOTS_PER_VIEW;

/// Why an attach was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachError {
    /// The slot number is not below [`SLOTS_PER_VIEW`].
    OutOfRange,
    /// The slot is busy: webd handed it out and the client has not released it.
    Busy,
}

struct Slot<M> {
    memory: M,
    /// What the attach declared, for the client's memory quota.
    bytes: u64,
    busy: bool,
}

/// A view's slots.
pub struct Slots<M> {
    slots: [Option<Slot<M>>; SLOTS_PER_VIEW],
}

impl<M> Default for Slots<M> {
    fn default() -> Self {
        Slots {
            slots: std::array::from_fn(|_| None),
        }
    }
}

impl<M> Slots<M> {
    /// Attach `memory` as slot `index`, free, replacing a free slot of that number (the old memory
    /// drops). `bytes` is what it counts for in [`Slots::bytes`].
    pub fn attach(&mut self, index: usize, memory: M, bytes: u64) -> Result<(), AttachError> {
        let slot = self.slots.get_mut(index).ok_or(AttachError::OutOfRange)?;
        if slot.as_ref().is_some_and(|slot| slot.busy) {
            return Err(AttachError::Busy);
        }
        *slot = Some(Slot {
            memory,
            bytes,
            busy: false,
        });
        Ok(())
    }

    /// Hand out the lowest free slot as busy and return its number.
    pub fn acquire(&mut self) -> Option<usize> {
        let index = self
            .slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|slot| !slot.busy))?;
        if let Some(slot) = &mut self.slots[index] {
            slot.busy = true;
        }
        Some(index)
    }

    /// Give a busy slot back (it is free again) and report whether it was busy; a free, empty or
    /// out-of-range slot is left alone.
    pub fn release(&mut self, index: usize) -> bool {
        match self.slots.get_mut(index) {
            Some(Some(slot)) if slot.busy => {
                slot.busy = false;
                true
            }
            _ => false,
        }
    }

    /// The memory of slot `index`.
    pub fn memory_mut(&mut self, index: usize) -> Option<&mut M> {
        match self.slots.get_mut(index) {
            Some(Some(slot)) => Some(&mut slot.memory),
            _ => None,
        }
    }

    /// Drop every slot, busy ones too (the view's size changed or the view ended).
    pub fn detach_all(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }

    /// Bytes the attached slots declared.
    pub fn bytes(&self) -> u64 {
        self.slots.iter().flatten().map(|slot| slot.bytes).sum()
    }

    /// Bytes the slot `index` declared, 0 when none.
    pub fn bytes_of(&self, index: usize) -> u64 {
        match self.slots.get(index) {
            Some(Some(slot)) => slot.bytes,
            _ => 0,
        }
    }

    /// Whether a frame could be placed now.
    pub fn has_free(&self) -> bool {
        self.slots.iter().flatten().any(|slot| !slot.busy)
    }

    /// Slots attached, and how many of them are busy.
    pub fn counts(&self) -> (usize, usize) {
        let attached = self.slots.iter().flatten().count();
        let busy = self.slots.iter().flatten().filter(|slot| slot.busy).count();
        (attached, busy)
    }

    pub fn is_busy(&self, index: usize) -> bool {
        matches!(self.slots.get(index), Some(Some(slot)) if slot.busy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_slot_is_free_and_the_lowest_free_one_is_handed_out() {
        let mut slots = Slots::default();
        assert_eq!(slots.acquire(), None);
        slots.attach(2, "c", 30).unwrap();
        slots.attach(0, "a", 10).unwrap();
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.acquire(), Some(2));
        assert_eq!(slots.acquire(), None);
        assert_eq!(slots.counts(), (2, 2));
        assert!(!slots.has_free());
    }

    #[test]
    fn a_released_slot_is_handed_out_again_and_a_free_one_is_not_released_twice() {
        let mut slots = Slots::default();
        slots.attach(0, (), 1).unwrap();
        slots.attach(1, (), 1).unwrap();
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.acquire(), Some(1));
        assert!(slots.release(0));
        assert!(!slots.release(0), "already free");
        assert!(!slots.release(2), "never attached");
        assert!(!slots.release(99), "out of range");
        assert_eq!(slots.acquire(), Some(0));
    }

    #[test]
    fn a_busy_slot_cannot_be_replaced_but_a_free_one_can() {
        let mut slots = Slots::default();
        slots.attach(0, 1, 10).unwrap();
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.attach(0, 2, 20), Err(AttachError::Busy));
        assert_eq!(slots.memory_mut(0), Some(&mut 1));
        assert!(slots.release(0));
        slots.attach(0, 3, 30).unwrap();
        assert_eq!(slots.memory_mut(0), Some(&mut 3));
        assert_eq!(slots.bytes(), 30);
        assert_eq!(
            slots.attach(SLOTS_PER_VIEW, 4, 1),
            Err(AttachError::OutOfRange)
        );
    }

    #[test]
    fn detach_all_drops_busy_slots_too_and_bytes_follow() {
        let mut slots = Slots::default();
        slots.attach(0, (), 5).unwrap();
        slots.attach(1, (), 7).unwrap();
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.bytes(), 12);
        slots.detach_all();
        assert_eq!((slots.bytes(), slots.counts()), (0, (0, 0)));
        assert_eq!(slots.acquire(), None);
        assert!(!slots.is_busy(0));
    }
}
