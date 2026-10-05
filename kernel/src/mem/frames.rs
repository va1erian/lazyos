//! The physical frame allocator: refcount table, free list and frame syscalls.
//!
//! The refcount rules, the free chain and the counters are `membook::frames`
//! (host-tested, issue #485); this module supplies the physical memory they
//! live in ([`PhysTable`]), the usable regions, the untouched frames and the
//! DMA pool.

use membook::frames::{FrameMemory, Ledger, Refused};

use super::dma::DmaPool;
use super::*;

/// Never hand out frames below this: the bootloader loads the kernel and its
/// metadata in the first megabyte.
pub(super) const LOWEST_FRAME: u64 = 0x10_0000;
/// Physical frame size; the unit of allocation and refcounting.
pub(super) use membook::frames::FRAME_SIZE;
/// Refcount value for frames the allocator owns itself and must never hand out
/// or free: the refcount side table.
pub(super) use membook::frames::RESERVED;

pub(super) static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

/// The physical frame allocator: a `u32` refcount side table plus an intrusive
/// free list threaded through the free frames' own memory.
///
/// The allocator cannot keep its metadata on the kernel heap: the heap is
/// mapped *using* frames during [`init`]. So `init` carves the refcount table
/// out of the first usable region and marks those frames [`RESERVED`]. Other
/// frames are handed out lazily in address order (`untouched`), and only
/// returned frames are linked into the free list. Both are reached through the
/// bootloader's physical-memory mapping ([`phys_to_virt`]).
///
/// Refcount values: `0` = free, `1..` = live, [`RESERVED`] = allocator
/// metadata. The table has one entry per 4 KiB frame up to the highest usable
/// address, so a frame's refcount is `table[phys / FRAME_SIZE]`.
pub(super) struct Frames {
    /// `(start, end)` of each usable region, clamped to [`LOWEST_FRAME`].
    pub(super) starts: [u64; MAX_REGIONS],
    pub(super) ends: [u64; MAX_REGIONS],
    pub(super) count: usize,
    /// Physical base of the refcount table (`u32` per frame).
    pub(super) refcounts: u64,
    /// The free chain and the allocation counters (`membook`).
    pub(super) ledger: Ledger,
    /// Frames never handed out, consumed lazily after the free list runs dry.
    pub(super) untouched: untouched::Untouched,
    /// Contiguous DMA region reserved at boot (issue #241). Its frames are in
    /// the refcount table but marked `RESERVED` while free, so the general
    /// allocator skips them; only [`Frames::dma_alloc`] hands them out.
    pub(super) pool: DmaPool,
    /// Frames the allocator can hand out (excludes reserved metadata and DMA
    /// pool frames; see [`FrameStats`]).
    pub(super) total: usize,
    /// Frames held back for the refcount table.
    pub(super) reserved: usize,
}

/// The refcount table and the free frames' link words, reached through the
/// bootloader's physical-memory mapping: the memory `membook` keeps its
/// books in.
#[derive(Clone, Copy)]
pub(super) struct PhysTable {
    /// Physical base of the refcount table.
    refcounts: u64,
}

impl PhysTable {
    fn refcount_ptr(self, phys: u64) -> *mut u32 {
        // Safety: `init` sized the table to cover every usable frame, and
        // the ledger only passes frames the caller found usable.
        unsafe {
            phys_to_virt(PhysAddr::new(self.refcounts))
                .as_mut_ptr::<u32>()
                .add(Frames::index(phys))
        }
    }
}

impl FrameMemory for PhysTable {
    fn refcount(&self, phys: u64) -> u32 {
        // Safety: see `refcount_ptr`.
        unsafe { self.refcount_ptr(phys).read_volatile() }
    }

    fn set_refcount(&mut self, phys: u64, value: u32) {
        // Safety: see `refcount_ptr`.
        unsafe { self.refcount_ptr(phys).write_volatile(value) }
    }

    fn link(&self, phys: u64) -> u64 {
        // Safety: the chain only links free usable frames, whose first word
        // holds the link `set_link` wrote.
        unsafe {
            phys_to_virt(PhysAddr::new(phys))
                .as_ptr::<u64>()
                .read_unaligned()
        }
    }

    fn set_link(&mut self, phys: u64, next: u64) {
        // Safety: `phys` is a free usable frame, so its first bytes are ours.
        unsafe {
            phys_to_virt(PhysAddr::new(phys))
                .as_mut_ptr::<u64>()
                .write_unaligned(next);
        }
    }
}

pub(super) static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// A snapshot of the frame allocator's counters.
///
/// [`FrameStats::live`] is the leak report: frames handed out and not yet
/// returned to the free pool. DMA pool frames are deliberately outside these
/// counters (they are a separate reserved region, tracked by
/// [`super::dma::DmaStats`]), so DMA traffic never perturbs a `live()` delta.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Frames the allocator can hand out (excludes reserved metadata and DMA
    /// pool frames).
    pub total: usize,
    /// Cumulative successful allocations.
    pub allocated: usize,
    /// Cumulative frees that returned a frame at reference count zero.
    pub freed: usize,
    /// Frames currently on the free list.
    pub free: usize,
    /// Frames reserved for the allocator's own metadata.
    pub reserved: usize,
    /// Double frees observed (should stay zero).
    pub double_frees: usize,
    /// Frees of non-usable addresses observed (should stay zero).
    pub invalid_frees: usize,
}

impl FrameStats {
    /// Frames currently handed out: the leak report (`allocated - freed`).
    pub fn live(&self) -> usize {
        self.allocated - self.freed
    }
}

/// Outcome of dropping one reference to a frame.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Release {
    /// The frame's reference count reached zero: it is back on the free list.
    Pooled,
    /// The frame is still referenced by someone else.
    Shared,
    /// Nothing was released: bad address or a double free (already reported).
    Invalid,
}

impl Frames {
    /// Frame index used by the refcount table.
    pub(super) fn index(phys: u64) -> usize {
        (phys / FRAME_SIZE) as usize
    }

    /// Whether `phys` lies in one of the usable regions.
    pub(super) fn contains(&self, phys: u64) -> bool {
        (0..self.count).any(|i| phys >= self.starts[i] && phys < self.ends[i])
    }

    /// The memory the ledger keeps its books in.
    pub(super) fn table(&self) -> PhysTable {
        PhysTable {
            refcounts: self.refcounts,
        }
    }

    /// The refcount of the usable frame at table index `index`.
    pub(super) fn refcount(&self, index: usize) -> u32 {
        self.table().refcount(index as u64 * FRAME_SIZE)
    }

    /// Set the refcount of the usable frame at table index `index`.
    pub(super) fn set_refcount(&self, index: usize, value: u32) {
        self.table().set_refcount(index as u64 * FRAME_SIZE, value);
    }

    /// Unlink and return the head of the free list, else the next frame that
    /// was never handed out (skipping the allocator's own reserved frames).
    pub(super) fn pop_free(&mut self) -> Option<u64> {
        if let Some(phys) = self.ledger.pop_free(&self.table()) {
            return Some(phys);
        }
        loop {
            let phys = self.untouched.next(&self.ends, self.count)?;
            if self.refcount(Self::index(phys)) != RESERVED && !self.pool.contains(phys) {
                return Some(phys);
            }
        }
    }

    /// Increment a live frame's reference count.
    pub(super) fn share(&mut self, phys: u64) -> bool {
        let usable = self.contains(phys);
        match self.ledger.share(&mut self.table(), phys, usable) {
            Ok(()) => true,
            Err(Refused::Unusable) => {
                crate::serial_println!("mem: share of non-usable frame {:#x}", phys);
                debug_assert!(false, "sharing a non-usable frame");
                false
            }
            Err(why) => {
                crate::serial_println!("mem: share of dead frame {:#x} ({:?})", phys, why);
                debug_assert!(false, "sharing a frame that is not live");
                false
            }
        }
    }

    /// Drop one reference to a frame, returning it to the free pool at zero.
    /// A DMA pool frame returns to the pool, not the general free list; the
    /// ledger marks it `RESERVED` so `pop_free` skips it and leaves it out of
    /// `total`/`allocated`/`freed`, so DMA traffic never moves `live()`.
    pub(super) fn release(&mut self, phys: u64) -> Release {
        let usable = self.contains(phys);
        let in_pool = usable && self.pool.contains(phys);
        match self
            .ledger
            .release(&mut self.table(), phys, usable, in_pool)
        {
            membook::frames::Release::Freed => Release::Pooled,
            membook::frames::Release::PoolFreed => {
                self.pool.free(phys);
                #[cfg(lazyos_tests)]
                super::dma::order::note(super::dma::order::DMA_FREE);
                Release::Pooled
            }
            membook::frames::Release::Shared(_) => Release::Shared,
            membook::frames::Release::Invalid(why) => {
                let what = match why {
                    Refused::Unusable => "non-usable",
                    Refused::DoubleFree => "already free (double free)",
                    _ => "reserved",
                };
                crate::serial_println!("mem: free of {} frame {:#x}", what, phys);
                debug_assert!(false, "invalid frame free");
                Release::Invalid
            }
        }
    }

    /// Allocate `pages` contiguous, zeroed frames from the DMA pool, aligned to
    /// `align_pages` pages. Sets each frame's refcount to one. `None` when the
    /// request does not fit a free run.
    pub(super) fn dma_alloc(&mut self, pages: u64, align_pages: u64) -> Option<u64> {
        let base = self.pool.alloc(pages, align_pages)?;
        for page in 0..pages {
            let phys = base + page * FRAME_SIZE;
            self.set_refcount(Self::index(phys), 1);
            // SAFETY: a freshly reserved pool frame is usable RAM, mapped
            // writable through the bootloader's physical-memory mapping, and
            // exclusively ours until handed to userspace.
            unsafe {
                core::ptr::write_bytes(
                    phys_to_virt(PhysAddr::new(phys)).as_mut_ptr::<u8>(),
                    0,
                    FRAME_SIZE as usize,
                );
            }
        }
        Some(base)
    }

    pub(super) fn stats(&self) -> FrameStats {
        FrameStats {
            total: self.total,
            allocated: self.ledger.allocated,
            freed: self.ledger.freed,
            free: self.total - self.ledger.live(),
            reserved: self.reserved,
            double_frees: self.ledger.double_frees,
            invalid_frees: self.ledger.invalid_frees,
        }
    }
}

/// Adapter so `map_to` can pull frames from the global allocator.
pub(super) struct GlobalFrames;

// Safety: `allocate_frame` only ever returns frames from `alloc_frame`, which
// hands out frames with a fresh refcount of one and never a frame still in
// use elsewhere — the contract `FrameAllocator` requires.
unsafe impl FrameAllocator<Size4KiB> for GlobalFrames {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        alloc_frame().map(PhysFrame::containing_address)
    }
}

/// Allocate one 4 KiB frame with a reference count of one.
pub fn alloc_frame() -> Option<PhysAddr> {
    let mut guard = FRAMES.lock();
    let frames = guard.as_mut()?;
    let phys = frames.pop_free()?;
    let mut table = frames.table();
    frames.ledger.note_alloc(&mut table, phys);
    Some(PhysAddr::new(phys))
}

/// The bootloader-provided physical memory offset.
pub fn physical_offset() -> VirtAddr {
    VirtAddr::new(PHYS_OFFSET.load(Ordering::Relaxed))
}

/// Convert a physical address to a kernel virtual address.
pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    physical_offset() + phys.as_u64()
}

/// Allocate a zeroed 4 KiB frame and return its physical address.
///
/// Each one is a poll point (`arch::irq_window`): loading a program, mapping
/// a stack or a shared buffer zeroes hundreds of frames in one syscall.
pub fn alloc_zeroed_frame() -> Option<PhysAddr> {
    crate::arch::irq_window::poll_point();
    let phys = alloc_frame()?;
    let virt = phys_to_virt(phys);
    // Safety: the frame is exclusively ours and mapped as writable.
    unsafe {
        core::ptr::write_bytes(virt.as_mut_ptr::<u8>(), 0, 4096);
    }
    Some(phys)
}

/// Current reference count of `phys`: `0` = free, [`RESERVED`] = allocator
/// metadata, `> 1` = shared between address spaces.
///
/// The diagnostics surface for tools (issue #54), and the copy-on-write
/// fault's sole-owner shortcut (`uspace::cow_fault`, P6.6).
pub fn frame_refcount(phys: PhysAddr) -> u32 {
    match FRAMES.lock().as_ref() {
        Some(frames) if frames.contains(phys.as_u64()) => {
            frames.refcount(Frames::index(phys.as_u64()))
        }
        _ => 0,
    }
}

/// Add a reference to a frame shared between address spaces (copy-on-write).
/// Returns false for addresses the allocator does not own or dead frames.
pub fn share_frame(phys: PhysAddr) -> bool {
    match FRAMES.lock().as_mut() {
        Some(frames) => frames.share(phys.as_u64()),
        None => false,
    }
}

/// Drop one reference to `phys`, returning the frame to the free pool when the
/// last reference goes away. Returns false (and reports) on a double free or a
/// non-usable address.
pub fn free_frame(phys: PhysAddr) -> bool {
    !matches!(release_frame(phys), Release::Invalid)
}

/// [`free_frame`] reporting whether the frame actually reached the free pool.
pub(super) fn release_frame(phys: PhysAddr) -> Release {
    match FRAMES.lock().as_mut() {
        Some(frames) => frames.release(phys.as_u64()),
        None => Release::Invalid,
    }
}

/// Snapshot of the allocator's global counters; see [`FrameStats`].
pub fn frame_stats() -> FrameStats {
    match FRAMES.lock().as_ref() {
        Some(frames) => frames.stats(),
        None => FrameStats::default(),
    }
}

/// The allocator's usable ranges, `(start, end)` in address order: the
/// coalesced boot map (`regions`), for `hwreport`-style summaries and tests.
#[allow(dead_code)] // Read by the kernel suite; a hardware report is the next caller.
pub fn usable_ranges() -> alloc::vec::Vec<(u64, u64)> {
    // Copy out first: the vector must not be allocated under the frame lock,
    // since the heap's slabs take frames from this allocator (P6.4).
    let Some((starts, ends, count)) = FRAMES
        .lock()
        .as_ref()
        .map(|frames| (frames.starts, frames.ends, frames.count))
    else {
        return alloc::vec::Vec::new();
    };
    (0..count).map(|i| (starts[i], ends[i])).collect()
}
