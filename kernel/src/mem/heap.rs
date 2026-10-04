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
//! # Small objects
//!
//! Requests of up to [`SMALL_MAX`] bytes (alignment included) are served from
//! per-size-class slabs (`mem::slab`'s classes, P6.4): a free-list pop and
//! push under one short lock, instead of a first-fit walk of a list that
//! fragments under churn. The slabs are whole frames from the frame
//! allocator, reached through the physical-memory map, so a pointer outside
//! [`HEAP_START`]'s span is a slab slot; frames stay with their class once
//! carved, like the list's high-water mark. Their bytes count against the
//! heap ceiling and in [`stats`], so the heap's accounting covers both
//! halves. When no frame is left the request falls back to the list.
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
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::slab::{Class, CLASSES};

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
        if let Some(class) = small_class(layout) {
            if let Some(ptr) = guarded(|| small_alloc(class)) {
                return ptr as *mut u8;
            }
        }
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
        if !in_list(ptr) {
            // Only `small_alloc` hands out pointers outside the list's span,
            // and only for a layout `small_class` accepts: the same layout
            // comes back here (the `GlobalAlloc` contract).
            if let Some(class) = small_class(layout) {
                guarded(|| small_free(class, ptr as usize));
            }
            return;
        }
        if let Some(ptr) = core::ptr::NonNull::new(ptr) {
            // SAFETY: forwarded with the caller's contract unchanged: `ptr`
            // came from `alloc` with this `layout`.
            guarded(|| unsafe { self.0.lock().deallocate(ptr, layout) });
        }
    }
}

/// Largest request (size or alignment) the slab classes serve.
pub const SMALL_MAX: usize = 2048;
/// The slab classes the heap uses: [`CLASSES`] up to [`SMALL_MAX`].
const SMALL_CLASSES: usize = 7;
const _: () = assert!(CLASSES[SMALL_CLASSES - 1] == SMALL_MAX);

/// The heap's own slab classes (not `mem::slab`'s typed-object ones).
static SMALL: Mutex<[Class; SMALL_CLASSES]> = Mutex::new([const { Class::new() }; SMALL_CLASSES]);

/// The slab class for `layout`: the smallest class holding its size and its
/// alignment (slots are aligned to their size inside page-aligned slabs).
fn small_class(layout: Layout) -> Option<usize> {
    let need = layout.size().max(layout.align()).max(1);
    CLASSES[..SMALL_CLASSES]
        .iter()
        .position(|&size| need <= size)
}

/// Whether `ptr` lies in the linked-list heap's span.
fn in_list(ptr: *mut u8) -> bool {
    (HEAP_START..HEAP_START + HEAP_SPAN).contains(&(ptr as u64))
}

/// Bytes of frames carved into the heap's slabs.
fn small_frames_bytes(classes: &[Class; SMALL_CLASSES]) -> u64 {
    classes.iter().map(|class| class.slabs as u64).sum::<u64>() * PAGE
}

/// Pop a slot of `class`, carving a new frame when the class is empty and
/// the ceiling allows; `None` sends the request to the list.
fn small_alloc(class: usize) -> Option<usize> {
    let mut classes = SMALL.lock();
    if let Some(slot) = classes[class].pop() {
        classes[class].live += 1;
        return Some(slot);
    }
    let ceiling = crate::limits::heap_max().min(HEAP_SPAN);
    let committed = MAPPED.load(Ordering::Relaxed) + small_frames_bytes(&classes);
    if committed + PAGE > ceiling || !classes[class].grow(class) {
        return None;
    }
    let slot = classes[class].pop()?;
    classes[class].live += 1;
    Some(slot)
}

/// Push a slot back onto `class`.
fn small_free(class: usize, slot: usize) {
    let mut classes = SMALL.lock();
    classes[class].live = classes[class].live.saturating_sub(1);
    classes[class].push(slot);
}

/// `(frame bytes, bytes handed out)` of the heap's slabs.
fn small_usage() -> (usize, usize) {
    let classes = SMALL.lock();
    let used = classes
        .iter()
        .zip(CLASSES)
        .map(|(class, size)| class.live * size)
        .sum();
    (small_frames_bytes(&classes) as usize, used)
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
    let ceiling = crate::limits::heap_max()
        .min(HEAP_SPAN)
        .saturating_sub(small_usage().0 as u64);
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

/// Snapshot the heap's counters without allocating: the list and the small
/// object slabs together.
pub fn stats() -> HeapStats {
    guarded(|| {
        let (slab_total, slab_used) = small_usage();
        let heap = ALLOCATOR.0.lock();
        HeapStats {
            total: heap.size() + slab_total,
            used: heap.used() + slab_used,
            free: heap.free() + (slab_total - slab_used),
            max: crate::limits::heap_max().min(HEAP_SPAN) as usize,
            growths: GROWTHS.load(Ordering::Relaxed),
            grow_failures: GROW_FAILURES.load(Ordering::Relaxed),
        }
    })
}
