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
