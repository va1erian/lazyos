//! Kernel heap and global allocator, enabling the `alloc` crate.

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialise the heap over an already-mapped virtual region.
///
/// # Safety
/// `start..start+size` must be mapped, writable, and not used for anything else.
pub unsafe fn init(start: usize, size: usize) {
    ALLOCATOR.lock().init(start as *mut u8, size);
}

/// Live usage of the linked-list heap, for the system-stats snapshot
/// (issue #144). The heap is the oversized-object fallback of the slab
/// allocator, so `used` is usually small.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes the heap owns.
    pub total: usize,
    /// Bytes currently handed out.
    pub used: usize,
    /// Bytes on the free list (`total - used`).
    pub free: usize,
}

/// Snapshot the heap's counters without allocating.
pub fn stats() -> HeapStats {
    let heap = ALLOCATOR.lock();
    HeapStats {
        total: heap.size(),
        used: heap.used(),
        free: heap.free(),
    }
}
