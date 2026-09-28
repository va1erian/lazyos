//! Slab allocator for common kernel objects (issue #61).
//!
//! The kernel heap ([`super::heap`]) is a general-purpose free-list allocator:
//! every allocation pays a header and external-fragmentation cost. Kernel
//! objects, on the other hand, come in a handful of shapes (handle entries,
//! queue nodes, VMAs), so they are better served by *slabs*: one 4 KiB frame
//! is carved into equal slots of a single size class and the free slots are
//! threaded into a per-class free list. Allocation is a pointer pop from that
//! list; freeing is a push back, so churn of millions of same-sized objects
//! neither fragments memory nor grows the working set.
//!
//! Classes are powers of two from 32 B to 4 KiB: a request is rounded up to
//! the smallest class that fits. Requests bigger than the largest class fall
//! back to the linked-list heap (the *oversized* path), so the allocator is
//! always usable without a second implementation.
//!
//! Ordering: the slab path needs only [`super::alloc_frame`] and
//! [`super::phys_to_virt`], which are live once `mem::init` has gathered the
//! memory map and the frame allocator. It is therefore usable for boot-time
//! kernel objects *before* the heap is mapped; only the oversized fallback has
//! to wait for `heap::init`. The allocator's own state is a const-initialised
//! static (a spin lock over plain counters), so no init call is required.
//!
//! Frames are never returned to the frame allocator: a class keeps the slabs it
//! has grown. Kernel object counts plateau, so this is a bounded cost; slab
//! shrinking is a follow-up. Double frees are caught only at class level (a
//! free with no live slots in its class is refused and counted); a per-slot
//! poison word would cost payload from every object, so precise detection is
//! left to the typed wrappers that will layer on this raw API.
//!
//! Accounting: [`stats`] reports live/peak bytes per class plus totals, and
//! [`charge`]/[`uncharge`] apportion bytes to an *owner* (a task slot) for the
//! per-process kernel-memory quotas in issue #61. Wiring handle/channel/VMA
//! allocation sites to charge/uncharge is a follow-up; the hook and its
//! saturating error behaviour are exercised by the kernel suite today.

#![allow(dead_code)] // Call sites migrate as subsystems land; the suite exercises the API today.

use alloc::alloc::{alloc_zeroed, dealloc as heap_dealloc};
use core::alloc::Layout;
use core::ptr::NonNull;
use spin::Mutex;

/// Size classes in bytes, ascending. Slot sizes divide the 4 KiB frame, so a
/// slab has no remainder.
pub const CLASSES: [usize; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

/// Number of size classes.
pub const CLASS_COUNT: usize = CLASSES.len();

/// Largest allocation served by a slab; bigger requests take the heap fallback.
pub const MAX_SLAB_SIZE: usize = CLASSES[CLASS_COUNT - 1];

/// Owners tracked by [`charge`]/[`uncharge`], indexed by task slot. Slot 0 is
/// the kernel task, so kernel objects are chargeable too.
pub const MAX_OWNERS: usize = crate::task::MAX_TASKS;

/// Alignment requested from the heap for oversized allocations: the smallest
/// class, which is also the guaranteed alignment of every slab slot.
const HEAP_ALIGN: usize = CLASSES[0];

/// Frame size. One frame backs exactly one slab in this simple design.
const FRAME_SIZE: usize = 4096;

/// Slot size of `class` in bytes.
///
/// # Panics
/// Panics when `class` is not a valid index into [`CLASSES`].
pub fn class_size(class: usize) -> usize {
    CLASSES[class]
}

/// Index of the smallest class that fits `bytes`, or `None` when the request
/// is larger than [`MAX_SLAB_SIZE`] (use [`alloc_bytes`] for that fallback).
pub fn class_for_size(bytes: usize) -> Option<usize> {
    CLASSES.iter().position(|&size| bytes <= size)
}

/// Slots carved out of one frame for `class`.
fn slots_per_slab(class: usize) -> usize {
    FRAME_SIZE / CLASSES[class]
}

/// Counters for one size class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassStats {
    /// Slot size in bytes.
    pub size: usize,
    /// Slots currently handed out.
    pub live: usize,
    /// High-water mark of [`ClassStats::live`].
    pub peak: usize,
    /// Slots parked on the free list.
    pub free: usize,
    /// Frames carved into slots for this class.
    pub slabs: usize,
    /// Cumulative successful allocations.
    pub allocations: usize,
    /// Cumulative frees that returned a slot to the free list.
    pub frees: usize,
}

/// Snapshot of the slab allocator, returned by [`stats`].
///
/// [`SlabStats::live_bytes`] is the leak report (bytes handed out and not yet
/// returned); [`SlabStats::peak_bytes`] is the high-water mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlabStats {
    /// Per-class counters, indexed like [`CLASSES`].
    pub classes: [ClassStats; CLASS_COUNT],
    /// Bytes live across all slab classes.
    pub live_bytes: usize,
    /// High-water mark of [`SlabStats::live_bytes`].
    pub peak_bytes: usize,
    /// Bytes live in the oversized heap fallback.
    pub oversized_bytes: usize,
    /// High-water mark of [`SlabStats::oversized_bytes`].
    pub oversized_peak_bytes: usize,
    /// Cumulative oversized allocations.
    pub oversized_allocations: usize,
    /// Cumulative oversized frees.
    pub oversized_frees: usize,
    /// Frees that found no live slot in their class (should stay zero).
    pub double_frees: usize,
    /// Failed [`charge`]/[`uncharge`] calls: bad owner or over-uncharge.
    pub accounting_errors: usize,
}

/// Kernel-memory accounting for one owner (task slot); see [`owner_stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnerStats {
    /// Bytes currently charged to the owner.
    pub live_bytes: usize,
    /// High-water mark of [`OwnerStats::live_bytes`].
    pub peak_bytes: usize,
    /// Cumulative successful [`charge`] calls.
    pub charges: usize,
    /// Cumulative successful [`uncharge`] calls.
    pub uncharges: usize,
}

/// One size class: a free list plus counters.
///
/// The free list is intrusive: a free slot's first word holds the virtual
/// address of the next free slot (`0` ends the list). That is why a slot is
/// never smaller than a pointer; the smallest class is 32 B.
struct Class {
    /// Virtual address of the free-list head, `0` when the list is empty.
    free: usize,
    /// Slots handed out and not returned.
    live: usize,
    /// High-water mark of [`Class::live`].
    peak: usize,
    /// Frames carved into this class.
    slabs: usize,
    /// Cumulative allocations.
    allocations: usize,
    /// Cumulative frees.
    frees: usize,
}

impl Class {
    const fn new() -> Self {
        Class {
            free: 0,
            live: 0,
            peak: 0,
            slabs: 0,
            allocations: 0,
            frees: 0,
        }
    }

    /// Pop the free-list head.
    fn pop(&mut self) -> Option<usize> {
        if self.free == 0 {
            return None;
        }
        let slot = self.free;
        // Safety: a free slot's first word holds the next link, written by
        // `push` when the slot was freed.
        self.free = unsafe { (slot as *const usize).read_unaligned() };
        Some(slot)
    }

    /// Push `slot` onto the free-list head.
    fn push(&mut self, slot: usize) {
        // Safety: the caller owns `slot` and only hands back slots of this
        // class, so overwriting its first word with the link is sound.
        unsafe { (slot as *mut usize).write_unaligned(self.free) };
        self.free = slot;
    }

    /// Carve one fresh frame into slots of `class` and link them all into the
    /// free list. Returns false when the frame allocator is out of memory, in
    /// which case the class is left untouched.
    fn grow(&mut self, class: usize) -> bool {
        let Some(phys) = super::alloc_frame() else {
            return false;
        };
        let base = super::phys_to_virt(phys).as_u64() as usize;
        for offset in (0..FRAME_SIZE).step_by(CLASSES[class]) {
            self.push(base + offset);
        }
        self.slabs += 1;
        true
    }
}

/// One owner's kernel-memory ledger.
struct Owner {
    live: usize,
    peak: usize,
    charges: usize,
    uncharges: usize,
}

impl Owner {
    const fn new() -> Self {
        Owner {
            live: 0,
            peak: 0,
            charges: 0,
            uncharges: 0,
        }
    }
}

/// The allocator: per-class free lists and counters, plus oversized and owner
/// accounting. One spin lock protects all of it; critical sections only touch
/// pointers and counters, except [`Class::grow`] and the heap fallback, which
/// call into other allocators (never the reverse, so no lock cycle exists).
struct Slab {
    classes: [Class; CLASS_COUNT],
    live_bytes: usize,
    peak_bytes: usize,
    oversized_live: usize,
    oversized_peak: usize,
    oversized_allocs: usize,
    oversized_frees: usize,
    double_frees: usize,
    accounting_errors: usize,
    owners: [Owner; MAX_OWNERS],
}

impl Slab {
    const fn new() -> Self {
        Slab {
            classes: [const { Class::new() }; CLASS_COUNT],
            live_bytes: 0,
            peak_bytes: 0,
            oversized_live: 0,
            oversized_peak: 0,
            oversized_allocs: 0,
            oversized_frees: 0,
            double_frees: 0,
            accounting_errors: 0,
            owners: [const { Owner::new() }; MAX_OWNERS],
        }
    }
}

static SLAB: Mutex<Slab> = Mutex::new(Slab::new());

/// Allocate one zeroed slot from `class`, an index into [`CLASSES`].
///
/// The slot is aligned to at least its class size (32 B). Returns `None` when
/// `class` is out of range or a new slab is needed and the frame allocator has
/// no free frame: the caller reports the failure (e.g. a friendly `ERR_QUOTA`),
/// nothing panics. Free the slot with [`dealloc`].
pub fn alloc(class: usize) -> Option<NonNull<u8>> {
    let size = *CLASSES.get(class)?;
    let mut slab = SLAB.lock();
    let state = &mut slab.classes[class];
    if state.free == 0 && !state.grow(class) {
        return None;
    }
    // INVARIANT: `state.grow` either returned `false` above (and we already
    // bailed out) or left at least one slot on the free list, so `pop` here
    // always has one to hand back.
    let slot = state.pop().expect("a grown class always has a free slot");
    state.live += 1;
    state.allocations += 1;
    state.peak = state.peak.max(state.live);
    slab.live_bytes += size;
    slab.peak_bytes = slab.peak_bytes.max(slab.live_bytes);
    // Kernel objects must not leak whatever the previous occupant stored.
    // Safety: the slot is exclusively ours and at least `size` bytes long.
    unsafe { core::ptr::write_bytes(slot as *mut u8, 0, size) };
    // Safety: `slot` is a live, non-null address.
    Some(unsafe { NonNull::new_unchecked(slot as *mut u8) })
}

/// Return a slot to its class.
///
/// # Safety
/// `ptr` must be a live slot obtained from [`alloc`] with the same `class`,
/// and it must not be used (or freed) again afterwards. Freeing a slot twice
/// corrupts the class free list; only a free that finds no live slot in the
/// class at all is caught and counted in [`SlabStats::double_frees`].
pub unsafe fn dealloc(class: usize, ptr: NonNull<u8>) {
    let Some(&size) = CLASSES.get(class) else {
        debug_assert!(false, "slab: dealloc with class {class} out of range");
        return;
    };
    debug_assert!(
        ptr.as_ptr() as usize % size == 0,
        "slab: dealloc of misaligned pointer {:#x}",
        ptr.as_ptr() as usize
    );
    let mut slab = SLAB.lock();
    let state = &mut slab.classes[class];
    if state.live == 0 {
        slab.double_frees += 1;
        crate::serial_println!("mem: slab double free in class {class}");
        debug_assert!(false, "slab: double free");
        return;
    }
    state.live -= 1;
    state.frees += 1;
    state.push(ptr.as_ptr() as usize);
    slab.live_bytes -= size;
}

/// Allocate at least `bytes` of zeroed kernel memory: a slab class when one
/// fits, otherwise the linked-list heap.
///
/// The oversized path follows the request literally (only alignment is raised
/// to 32 B) and is accounted separately. Returns `None` on out-of-memory or an
/// impossible layout; it never panics. Free with [`dealloc_bytes`] and the
/// same `bytes` value.
pub fn alloc_bytes(bytes: usize) -> Option<NonNull<u8>> {
    match class_for_size(bytes) {
        Some(class) => alloc(class),
        None => alloc_oversized(bytes),
    }
}

/// Return an allocation made by [`alloc_bytes`].
///
/// # Safety
/// `ptr` must be a live allocation from [`alloc_bytes`] called with the same
/// `bytes`, and it must not be used again afterwards.
pub unsafe fn dealloc_bytes(bytes: usize, ptr: NonNull<u8>) {
    match class_for_size(bytes) {
        Some(class) => dealloc(class, ptr),
        None => dealloc_oversized(bytes, ptr),
    }
}

/// Layout for an oversized request; `None` when the size cannot be expressed.
fn heap_layout(bytes: usize) -> Option<Layout> {
    Layout::from_size_align(bytes, HEAP_ALIGN).ok()
}

/// Heap fallback for requests bigger than [`MAX_SLAB_SIZE`].
fn alloc_oversized(bytes: usize) -> Option<NonNull<u8>> {
    let layout = heap_layout(bytes)?;
    // The global allocator is called outside the slab lock: it is a separate
    // allocator and holding two locks here would invite a deadlock later.
    // Safety: `layout` has a non-zero size (checked by `heap_layout`'s caller
    // contract: `bytes` is always > `MAX_SLAB_SIZE` > 0 here).
    let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
    let mut slab = SLAB.lock();
    slab.oversized_live += bytes;
    slab.oversized_peak = slab.oversized_peak.max(slab.oversized_live);
    slab.oversized_allocs += 1;
    Some(ptr)
}

/// Return an oversized allocation to the heap.
///
/// # Safety
/// `ptr` must have come from [`alloc_oversized`] with the same `bytes`.
unsafe fn dealloc_oversized(bytes: usize, ptr: NonNull<u8>) {
    let Some(layout) = heap_layout(bytes) else {
        debug_assert!(false, "slab: oversized dealloc with an impossible size");
        return;
    };
    {
        let mut slab = SLAB.lock();
        slab.oversized_live = slab.oversized_live.saturating_sub(bytes);
        slab.oversized_frees += 1;
    }
    // Safety: the caller guarantees `ptr` came from `alloc_zeroed` with this
    // exact layout.
    unsafe { heap_dealloc(ptr.as_ptr(), layout) };
}

/// Snapshot of the allocator's counters; see [`SlabStats`]. Cheap to call.
pub fn stats() -> SlabStats {
    let slab = SLAB.lock();
    let mut classes = [ClassStats::default(); CLASS_COUNT];
    for class in 0..CLASS_COUNT {
        let state = &slab.classes[class];
        classes[class] = ClassStats {
            size: CLASSES[class],
            live: state.live,
            peak: state.peak,
            free: state.slabs * slots_per_slab(class) - state.live,
            slabs: state.slabs,
            allocations: state.allocations,
            frees: state.frees,
        };
    }
    SlabStats {
        classes,
        live_bytes: slab.live_bytes,
        peak_bytes: slab.peak_bytes,
        oversized_bytes: slab.oversized_live,
        oversized_peak_bytes: slab.oversized_peak,
        oversized_allocations: slab.oversized_allocs,
        oversized_frees: slab.oversized_frees,
        double_frees: slab.double_frees,
        accounting_errors: slab.accounting_errors,
    }
}

/// Charge `bytes` of kernel memory to `owner` (a task slot; 0 is the kernel).
///
/// Returns false when `owner` is out of range, which is also counted in
/// [`SlabStats::accounting_errors`]. Quota *enforcement* builds on this plus
/// [`owner_stats`]; it lands with the IPC call sites (issue #61 follow-up).
pub fn charge(owner: usize, bytes: usize) -> bool {
    if owner >= MAX_OWNERS {
        SLAB.lock().accounting_errors += 1;
        return false;
    }
    let mut slab = SLAB.lock();
    let state = &mut slab.owners[owner];
    state.live += bytes;
    state.peak = state.peak.max(state.live);
    state.charges += 1;
    true
}

/// Release `bytes` previously charged to `owner`.
///
/// Returns false when `owner` is out of range or `bytes` exceeds the owner's
/// live charge; either way the live value saturates at zero (it never wraps)
/// and [`SlabStats::accounting_errors`] is incremented.
pub fn uncharge(owner: usize, bytes: usize) -> bool {
    if owner >= MAX_OWNERS {
        SLAB.lock().accounting_errors += 1;
        return false;
    }
    let mut slab = SLAB.lock();
    let state = &mut slab.owners[owner];
    if bytes > state.live {
        state.live = 0;
        slab.accounting_errors += 1;
        return false;
    }
    state.live -= bytes;
    state.uncharges += 1;
    true
}

/// Live/peak/charge counters for `owner`, or `None` for an invalid slot.
pub fn owner_stats(owner: usize) -> Option<OwnerStats> {
    let slab = SLAB.lock();
    slab.owners.get(owner).map(|state| OwnerStats {
        live_bytes: state.live,
        peak_bytes: state.peak,
        charges: state.charges,
        uncharges: state.uncharges,
    })
}
