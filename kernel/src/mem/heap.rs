//! Kernel heap and global allocator, enabling the `alloc` crate.
//!
//! # Growth
//!
//! The heap lives in its own PML4 entry of the kernel half ([`HEAP_START`],
//! [`HEAP_SPAN`]). `mem::init` maps an initial size derived from RAM
//! ([`crate::limits::heap_initial_bytes`]); when an allocation finds no hole,
//! [`grow`] maps more frames at the top and extends the free list, up to
//! [`crate::limits::heap_max`]. Freed memory stays with the heap (the
//! linked-list allocator cannot shrink), which is the usual kernel-heap
//! trade-off: the high-water mark is what the kernel needed once.
//!
//! Growth maps into the PDPT under the heap's PML4 entry, which `mem::init`
//! creates before any address space exists. Every user PML4 copies that entry
//! (`new_user_table`), so pages added later are visible in every address
//! space at once; [`HEAP_SPAN`] is one PML4 entry precisely so this holds.
//!
//! # Interrupts
//!
//! The heap lock is only ever held with interrupts off (issue #382). The
//! kernel mux allocates from a preemptible context, while syscalls (and the
//! scheduler's signal sweep) allocate with interrupts off. A tick that
//! preempted the mux inside the allocator would leave the lock held while the
//! next task's allocation spun on it with the timer masked: a silent,
//! permanent hang. Masking interrupts for the few hundred cycles an
//! allocation holds the lock makes a preempted holder impossible on this
//! single CPU. Growth runs inside the same masked section but with the heap
//! lock released, taking only the frame allocator's lock.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;

/// Virtual base of the kernel heap: PML4 entry 384, in the kernel half and
/// outside the range the bootloader places its own mappings in
/// (`kernel_main`'s `BootloaderConfig`).
pub const HEAP_START: u64 = 0xffff_c000_0000_0000;
/// Virtual span reserved for the heap: one PML4 entry (512 GiB).
pub const HEAP_SPAN: u64 = 1 << 39;
/// Smallest growth step: big enough that a burst of small allocations does
/// not map one page at a time.
const GROW_STEP: u64 = 4 << 20;
const PAGE: u64 = 4096;

/// [`LockedHeap`] whose lock is taken with interrupts disabled and which
/// grows on demand.
struct IrqSafeHeap(LockedHeap);

// SAFETY: allocation and deallocation forward to `linked_list_allocator`'s
// `Heap` with the caller's layout unchanged; growth only appends freshly
// mapped, exclusively owned memory directly above the heap's current top.
unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        guarded(|| loop {
            if let Ok(ptr) = self.0.lock().allocate_first_fit(layout) {
                return ptr.as_ptr();
            }
            if !grow(layout) {
                return core::ptr::null_mut();
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(ptr) = core::ptr::NonNull::new(ptr) {
            // SAFETY: forwarded with the caller's contract unchanged: `ptr`
            // came from `alloc` with this `layout`.
            guarded(|| unsafe { self.0.lock().deallocate(ptr, layout) });
        }
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

/// Bytes mapped for the heap so far ([`HEAP_START`]-relative end).
static MAPPED: AtomicU64 = AtomicU64::new(0);
/// Successful growth steps, and requests growth could not satisfy.
static GROWTHS: AtomicU64 = AtomicU64::new(0);
static GROW_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Initialise the heap over the `size` bytes already mapped at [`HEAP_START`].
///
/// # Safety
/// `HEAP_START..HEAP_START + size` must be mapped, writable, and not used for
/// anything else.
pub unsafe fn init(size: u64) {
    MAPPED.store(size, Ordering::Relaxed);
    // SAFETY: the caller's contract is `Heap::init`'s.
    guarded(|| unsafe {
        ALLOCATOR
            .0
            .lock()
            .init(HEAP_START as *mut u8, size as usize)
    });
}

/// Map enough pages above the heap's top to serve `layout`, and hand them to
/// the allocator. False when the heap is at its ceiling or RAM is exhausted.
/// Runs with interrupts off and the heap lock released.
fn grow(layout: Layout) -> bool {
    let mapped = MAPPED.load(Ordering::Relaxed);
    let ceiling = crate::limits::heap_max().min(HEAP_SPAN);
    // The allocator needs the payload, alignment slack and a hole header.
    let need = (layout.size() as u64)
        .saturating_add(layout.align() as u64)
        .saturating_add(64);
    let need = need.saturating_add(PAGE - 1) & !(PAGE - 1);
    let step = need.max(GROW_STEP).min(ceiling.saturating_sub(mapped));
    if step < need {
        GROW_FAILURES.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let added = super::map_kernel_range(HEAP_START + mapped, step / PAGE) * PAGE;
    if added == 0 {
        GROW_FAILURES.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    MAPPED.store(mapped + added, Ordering::Relaxed);
    // SAFETY: `HEAP_START + mapped .. + added` was just mapped writable, lies
    // directly above the heap's current top (`MAPPED` tracks it), and nothing
    // else uses the heap's virtual span.
    unsafe { ALLOCATOR.0.lock().extend(added as usize) };
    GROWTHS.fetch_add(1, Ordering::Relaxed);
    added >= need
}

/// Whether the heap lock is held right now (the NMI hang report, issue #382).
pub fn locked() -> bool {
    ALLOCATOR.0.is_locked()
}

/// Live usage of the linked-list heap, for the system-stats snapshot
/// (issue #144).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes the heap owns (mapped so far).
    pub total: usize,
    /// Bytes currently handed out.
    pub used: usize,
    /// Bytes on the free list (`total - used`).
    pub free: usize,
    /// The ceiling growth may reach ([`crate::limits::heap_max`]).
    pub max: usize,
    /// Growth steps taken since boot.
    pub growths: u64,
    /// Allocations that failed because the heap could not grow.
    pub grow_failures: u64,
}

/// Snapshot the heap's counters without allocating.
pub fn stats() -> HeapStats {
    guarded(|| {
        let heap = ALLOCATOR.0.lock();
        HeapStats {
            total: heap.size(),
            used: heap.used(),
            free: heap.free(),
            max: crate::limits::heap_max().min(HEAP_SPAN) as usize,
            growths: GROWTHS.load(Ordering::Relaxed),
            grow_failures: GROW_FAILURES.load(Ordering::Relaxed),
        }
    })
}
