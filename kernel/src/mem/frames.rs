//! The physical frame allocator: refcount table, free list and frame syscalls.

use super::*;

/// Never hand out frames below this: the bootloader loads the kernel and its
/// metadata in the first megabyte.
pub(super) const LOWEST_FRAME: u64 = 0x10_0000;
/// Physical frame size; the unit of allocation and refcounting.
pub(super) const FRAME_SIZE: u64 = 4096;
/// Refcount value for frames the allocator owns itself and must never hand out
/// or free: the refcount side table.
pub(super) const RESERVED: u32 = u32::MAX;
/// An empty free list's head. Physical address 0 is never a usable frame (they
/// start at [`LOWEST_FRAME`]), so it is a safe sentinel.
pub(super) const FREE_LIST_END: u64 = 0;

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
    /// Physical address of the first free frame ([`FREE_LIST_END`] if none).
    pub(super) free_head: u64,
    /// Frames never handed out, consumed lazily after the free list runs dry.
    pub(super) untouched: untouched::Untouched,
    /// Frames the allocator can hand out (excludes reserved metadata frames).
    pub(super) total: usize,
    /// Cumulative successful allocations.
    pub(super) allocated: usize,
    /// Cumulative frees that returned a frame to the free pool.
    pub(super) freed: usize,
    /// Frames held back for the refcount table.
    pub(super) reserved: usize,
    /// Frees of an already-free frame (a bug indicator; should stay zero).
    pub(super) double_frees: usize,
    /// Frees of an address outside every usable region (should stay zero).
    pub(super) invalid_frees: usize,
}

pub(super) static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// A snapshot of the frame allocator's counters.
///
/// [`FrameStats::live`] is the leak report: frames handed out and not yet
/// returned to the free pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Frames the allocator can hand out (excludes reserved metadata frames).
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

    pub(super) fn refcount_ptr(&self, index: usize) -> *mut u32 {
        // Safety: `init` sized the table to cover every usable frame, and
        // callers only pass indices derived from usable physical addresses.
        unsafe {
            phys_to_virt(PhysAddr::new(self.refcounts))
                .as_mut_ptr::<u32>()
                .add(index)
        }
    }

    pub(super) fn refcount(&self, index: usize) -> u32 {
        // Safety: see `refcount_ptr`.
        unsafe { self.refcount_ptr(index).read_volatile() }
    }

    pub(super) fn set_refcount(&self, index: usize, value: u32) {
        // Safety: see `refcount_ptr`.
        unsafe { self.refcount_ptr(index).write_volatile(value) }
    }

    /// Link `phys` at the head of the free list.
    pub(super) fn push_free(&mut self, phys: u64) {
        // Safety: `phys` is a free usable frame, so its first bytes are ours.
        unsafe {
            phys_to_virt(PhysAddr::new(phys))
                .as_mut_ptr::<u64>()
                .write_unaligned(self.free_head);
        }
        self.free_head = phys;
    }

    /// Unlink and return the head of the free list, else the next frame that
    /// was never handed out (skipping the allocator's own reserved frames).
    pub(super) fn pop_free(&mut self) -> Option<u64> {
        if self.free_head == FREE_LIST_END {
            loop {
                let phys = self.untouched.next(&self.ends, self.count)?;
                if self.refcount(Self::index(phys)) != RESERVED {
                    return Some(phys);
                }
            }
        }
        let phys = self.free_head;
        // Safety: the free list only links free usable frames.
        self.free_head = unsafe {
            phys_to_virt(PhysAddr::new(phys))
                .as_ptr::<u64>()
                .read_unaligned()
        };
        Some(phys)
    }

    /// Increment a live frame's reference count.
    pub(super) fn share(&mut self, phys: u64) -> bool {
        if phys & (FRAME_SIZE - 1) != 0 || !self.contains(phys) {
            crate::serial_println!("mem: share of non-usable frame {:#x}", phys);
            debug_assert!(false, "sharing a non-usable frame");
            return false;
        }
        let index = Self::index(phys);
        let count = self.refcount(index);
        if count == 0 || count == RESERVED || count >= RESERVED - 1 {
            crate::serial_println!("mem: share of dead frame {:#x} (refcount {})", phys, count);
            debug_assert!(false, "sharing a frame that is not live");
            return false;
        }
        self.set_refcount(index, count + 1);
        true
    }

    /// Drop one reference to a frame, returning it to the free pool at zero.
    pub(super) fn release(&mut self, phys: u64) -> Release {
        if phys & (FRAME_SIZE - 1) != 0 || !self.contains(phys) {
            self.invalid_frees += 1;
            crate::serial_println!("mem: free of non-usable frame {:#x}", phys);
            debug_assert!(false, "freeing a non-usable frame");
            return Release::Invalid;
        }
        let index = Self::index(phys);
        let count = self.refcount(index);
        if count == 0 {
            self.double_frees += 1;
            crate::serial_println!("mem: double free of frame {:#x}", phys);
            debug_assert!(false, "double free");
            return Release::Invalid;
        }
        if count == RESERVED {
            self.invalid_frees += 1;
            crate::serial_println!("mem: free of reserved frame {:#x}", phys);
            debug_assert!(false, "freeing a reserved frame");
            return Release::Invalid;
        }
        let remaining = count - 1;
        self.set_refcount(index, remaining);
        if remaining == 0 {
            self.push_free(phys);
            self.freed += 1;
            Release::Pooled
        } else {
            Release::Shared
        }
    }

    pub(super) fn stats(&self) -> FrameStats {
        FrameStats {
            total: self.total,
            allocated: self.allocated,
            freed: self.freed,
            free: self.total - (self.allocated - self.freed),
            reserved: self.reserved,
            double_frees: self.double_frees,
            invalid_frees: self.invalid_frees,
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
    frames.set_refcount(Frames::index(phys), 1);
    frames.allocated += 1;
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
pub fn alloc_zeroed_frame() -> Option<PhysAddr> {
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
/// Part of the diagnostics surface for tools (issue #54); the kernel itself
/// only reads refcounts through the allocator.
#[allow(dead_code)]
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
