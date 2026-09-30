//! Kernel heap and global allocator, enabling the `alloc` crate.
//!
//! The heap lock is only ever held with interrupts off (issue #382). The
//! kernel mux allocates from a preemptible context, while syscalls (and the
//! scheduler's signal sweep) allocate with interrupts off. A tick that
//! preempted the mux inside the allocator would leave the lock held while the
//! next task's allocation spun on it with the timer masked: a silent,
//! permanent hang. Masking interrupts for the few hundred cycles an
//! allocation holds the lock makes a preempted holder impossible on this
//! single CPU.

use core::alloc::{GlobalAlloc, Layout};

use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;

/// [`LockedHeap`] whose lock is taken with interrupts disabled.
struct IrqSafeHeap(LockedHeap);

// SAFETY: every method forwards to `LockedHeap`'s own `GlobalAlloc`
// implementation with the same arguments; masking interrupts around the call
// changes no allocation semantics.
unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded with the caller's contract unchanged.
        guarded(|| unsafe { self.0.alloc(layout) })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded with the caller's contract unchanged.
        guarded(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

/// Run one heap critical section with interrupts off.
fn guarded<R>(f: impl FnOnce() -> R) -> R {
    without_interrupts(|| {
        #[cfg(lazyos_tests)]
        harness::note();
        f()
    })
}

/// Test-harness view of the guard (issue #382): how many heap critical
/// sections ran, and how many of those ran with interrupts still enabled.
#[cfg(lazyos_tests)]
pub mod harness {
    use core::sync::atomic::{AtomicU64, Ordering};

    static SECTIONS: AtomicU64 = AtomicU64::new(0);
    static UNMASKED: AtomicU64 = AtomicU64::new(0);

    pub(super) fn note() {
        SECTIONS.fetch_add(1, Ordering::Relaxed);
        if x86_64::instructions::interrupts::are_enabled() {
            UNMASKED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(sections, unmasked)` since the last call; resets both.
    pub fn take() -> (u64, u64) {
        (
            SECTIONS.swap(0, Ordering::Relaxed),
            UNMASKED.swap(0, Ordering::Relaxed),
        )
    }
}

#[global_allocator]
static ALLOCATOR: IrqSafeHeap = IrqSafeHeap(LockedHeap::empty());

/// Initialise the heap over an already-mapped virtual region.
///
/// # Safety
/// `start..start+size` must be mapped, writable, and not used for anything else.
pub unsafe fn init(start: usize, size: usize) {
    // SAFETY: the caller's contract is `Heap::init`'s.
    guarded(|| unsafe { ALLOCATOR.0.lock().init(start as *mut u8, size) });
}

/// Whether the heap lock is held right now (the NMI hang report, issue #382).
pub fn locked() -> bool {
    ALLOCATOR.0.is_locked()
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
    guarded(|| {
        let heap = ALLOCATOR.0.lock();
        HeapStats {
            total: heap.size(),
            used: heap.used(),
            free: heap.free(),
        }
    })
}
