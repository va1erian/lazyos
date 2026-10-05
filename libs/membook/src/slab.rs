//! Slab size classes, the intrusive free list, and the slab ledgers.
//!
//! A slab is one [`FRAME_SIZE`] page carved into equal slots of one size
//! class. A free slot's first word holds the pointer to the next free slot,
//! so a class needs no memory of its own beyond the list head and counters;
//! that is why the smallest class is larger than a pointer.

use core::ptr::NonNull;

/// Size classes in bytes, ascending. Slot sizes divide the page, so a slab
/// has no remainder.
pub const CLASSES: [usize; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

/// Number of size classes.
pub const CLASS_COUNT: usize = CLASSES.len();

/// Largest allocation a slab serves.
pub const MAX_SLAB_SIZE: usize = CLASSES[CLASS_COUNT - 1];

/// Bytes of the page one slab is carved from.
pub const FRAME_SIZE: usize = 4096;

const _: () = {
    assert!(CLASSES[0] >= core::mem::size_of::<usize>());
    let mut class = 0;
    while class < CLASS_COUNT {
        assert!(FRAME_SIZE.is_multiple_of(CLASSES[class]));
        assert!(CLASSES[class].is_power_of_two());
        class += 1;
    }
};

/// Index of the smallest class that fits `bytes`, or `None` when the request
/// is larger than [`MAX_SLAB_SIZE`].
pub fn class_for_size(bytes: usize) -> Option<usize> {
    CLASSES.iter().position(|&size| bytes <= size)
}

/// The smallest class holding both `size` and `align` (slots are aligned to
/// their size inside page-aligned slabs), or `None` when none does.
pub fn class_for_layout(size: usize, align: usize) -> Option<usize> {
    class_for_size(size.max(align).max(1))
}

/// Slots carved out of one page for `class`.
///
/// # Panics
/// Panics when `class` is not an index into [`CLASSES`].
pub fn slots_per_slab(class: usize) -> usize {
    FRAME_SIZE / CLASSES[class]
}

/// A free with no live slot in its class: certainly a double free (or a free
/// to the wrong class). The slot was not linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoubleFree;

/// One size class: the free list and its counters.
///
/// Invariant: every slot on the list lies in a page handed to [`Class::carve`]
/// for this class, is not handed out, and holds the link to the next one.
pub struct Class {
    /// Free-list head, `None` when the list is empty.
    free: Option<NonNull<u8>>,
    /// Slots handed out and not returned.
    pub live: usize,
    /// High-water mark of [`Class::live`].
    pub peak: usize,
    /// Pages carved into this class.
    pub slabs: usize,
    /// Cumulative allocations.
    pub allocations: usize,
    /// Cumulative frees.
    pub frees: usize,
}

// SAFETY: the list only links slots this class owns exclusively (the type
// invariant); moving the class to another thread moves that ownership with
// it, and every access goes through `&mut self`.
unsafe impl Send for Class {}

impl Class {
    pub const fn new() -> Self {
        Class {
            free: None,
            live: 0,
            peak: 0,
            slabs: 0,
            allocations: 0,
            frees: 0,
        }
    }

    /// Whether the class has a free slot (no page needs carving).
    pub fn has_free(&self) -> bool {
        self.free.is_some()
    }

    /// Slots of this class on the free list, from the counters.
    pub fn free_slots(&self, class: usize) -> usize {
        self.slabs * slots_per_slab(class) - self.live
    }

    /// Thread every slot of `page` onto the free list.
    ///
    /// # Safety
    /// `page` must be valid for reads and writes of [`FRAME_SIZE`] bytes,
    /// aligned to `CLASSES[class]`, and owned by this class from now on: no
    /// one else may touch it, and it is never carved twice. `class` must be
    /// the class this list serves.
    pub unsafe fn carve(&mut self, page: NonNull<u8>, class: usize) {
        for offset in (0..FRAME_SIZE).step_by(CLASSES[class]) {
            // SAFETY: `offset < FRAME_SIZE`, so the slot lies inside `page`,
            // which the caller hands to this class.
            unsafe { self.push(page.add(offset)) };
        }
        self.slabs += 1;
    }

    /// Unlink the free-list head, without touching the counters.
    pub fn pop(&mut self) -> Option<NonNull<u8>> {
        let slot = self.free?;
        // SAFETY: a listed slot is ours and holds the next link, written by
        // `push` (the type invariant); an unaligned read keeps this sound
        // whatever the caller's page alignment was.
        self.free = unsafe { slot.cast::<Option<NonNull<u8>>>().read_unaligned() };
        Some(slot)
    }

    /// Link `slot` at the free-list head, without touching the counters.
    ///
    /// # Safety
    /// `slot` must be a slot of this class (from [`Class::carve`]'s pages)
    /// that is not on the list and that nobody uses any more.
    pub unsafe fn push(&mut self, slot: NonNull<u8>) {
        // SAFETY: the caller hands back an unused slot of this class, at
        // least a pointer wide (the smallest class is), so its first word
        // may hold the link.
        unsafe {
            slot.cast::<Option<NonNull<u8>>>()
                .write_unaligned(self.free)
        };
        self.free = Some(slot);
    }

    /// Hand out a free slot and count it; `None` when the list is empty.
    pub fn alloc(&mut self) -> Option<NonNull<u8>> {
        let slot = self.pop()?;
        self.live += 1;
        self.allocations += 1;
        self.peak = self.peak.max(self.live);
        Some(slot)
    }

    /// Take a slot back and count it. A class with no live slot refuses the
    /// free ([`DoubleFree`]) and leaves the list untouched.
    ///
    /// # Safety
    /// As [`Class::push`]: `slot` came from [`Class::alloc`] on this class
    /// and is not used again.
    pub unsafe fn free(&mut self, slot: NonNull<u8>) -> Result<(), DoubleFree> {
        if self.live == 0 {
            return Err(DoubleFree);
        }
        self.live -= 1;
        self.frees += 1;
        // SAFETY: the caller's contract is `push`'s.
        unsafe { self.push(slot) };
        Ok(())
    }
}

impl Default for Class {
    fn default() -> Self {
        Class::new()
    }
}

/// Kernel memory charged to one owner (a task slot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Owner {
    /// Bytes currently charged.
    pub live: usize,
    /// High-water mark of [`Owner::live`].
    pub peak: usize,
    /// Cumulative successful charges.
    pub charges: usize,
    /// Cumulative successful uncharges.
    pub uncharges: usize,
}

impl Owner {
    pub const fn new() -> Self {
        Owner {
            live: 0,
            peak: 0,
            charges: 0,
            uncharges: 0,
        }
    }

    /// Charge `bytes`. The live total saturates rather than wraps.
    pub fn charge(&mut self, bytes: usize) {
        self.live = self.live.saturating_add(bytes);
        self.peak = self.peak.max(self.live);
        self.charges += 1;
    }

    /// Release `bytes`. More than is charged is an accounting error: the live
    /// total drops to zero (never wraps) and the call returns false.
    pub fn uncharge(&mut self, bytes: usize) -> bool {
        if bytes > self.live {
            self.live = 0;
            return false;
        }
        self.live -= bytes;
        self.uncharges += 1;
        true
    }
}
