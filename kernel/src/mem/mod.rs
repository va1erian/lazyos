//! Memory management: physical frames, kernel paging, the slab allocator, and
//! the heap.

mod heap;
pub mod pte;
pub mod slab;
mod table_guard;
pub mod untouched;
pub mod vma;
pub use table_guard::UserTableGuard;

use bootloader_api::info::{MemoryRegionKind, Optional};
use bootloader_api::BootInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::error::{kstop, KError};

/// Virtual base of the kernel heap.
pub const HEAP_START: u64 = 0x_4444_4444_0000;
/// Size of the kernel heap (16 MiB): enough for a full-screen RGBA pixmap.
pub const HEAP_SIZE: u64 = 16 * 1024 * 1024;

/// Never hand out frames below this: the bootloader loads the kernel and its
/// metadata in the first megabyte.
const LOWEST_FRAME: u64 = 0x10_0000;
/// Maximum usable memory regions we track (no heap needed to bootstrap).
pub const MAX_REGIONS: usize = 32;
/// Physical frame size; the unit of allocation and refcounting.
const FRAME_SIZE: u64 = 4096;
/// Refcount value for frames the allocator owns itself and must never hand out
/// or free: the refcount side table.
const RESERVED: u32 = u32::MAX;
/// An empty free list's head. Physical address 0 is never a usable frame (they
/// start at [`LOWEST_FRAME`]), so it is a safe sentinel.
const FREE_LIST_END: u64 = 0;

static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

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
struct Frames {
    /// `(start, end)` of each usable region, clamped to [`LOWEST_FRAME`].
    starts: [u64; MAX_REGIONS],
    ends: [u64; MAX_REGIONS],
    count: usize,
    /// Physical base of the refcount table (`u32` per frame).
    refcounts: u64,
    /// Physical address of the first free frame ([`FREE_LIST_END`] if none).
    free_head: u64,
    /// Frames never handed out, consumed lazily after the free list runs dry.
    untouched: untouched::Untouched,
    /// Frames the allocator can hand out (excludes reserved metadata frames).
    total: usize,
    /// Cumulative successful allocations.
    allocated: usize,
    /// Cumulative frees that returned a frame to the free pool.
    freed: usize,
    /// Frames held back for the refcount table.
    reserved: usize,
    /// Frees of an already-free frame (a bug indicator; should stay zero).
    double_frees: usize,
    /// Frees of an address outside every usable region (should stay zero).
    invalid_frees: usize,
}

static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

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
enum Release {
    /// The frame's reference count reached zero: it is back on the free list.
    Pooled,
    /// The frame is still referenced by someone else.
    Shared,
    /// Nothing was released: bad address or a double free (already reported).
    Invalid,
}

impl Frames {
    /// Frame index used by the refcount table.
    fn index(phys: u64) -> usize {
        (phys / FRAME_SIZE) as usize
    }

    /// Whether `phys` lies in one of the usable regions.
    fn contains(&self, phys: u64) -> bool {
        (0..self.count).any(|i| phys >= self.starts[i] && phys < self.ends[i])
    }

    fn refcount_ptr(&self, index: usize) -> *mut u32 {
        // Safety: `init` sized the table to cover every usable frame, and
        // callers only pass indices derived from usable physical addresses.
        unsafe {
            phys_to_virt(PhysAddr::new(self.refcounts))
                .as_mut_ptr::<u32>()
                .add(index)
        }
    }

    fn refcount(&self, index: usize) -> u32 {
        // Safety: see `refcount_ptr`.
        unsafe { self.refcount_ptr(index).read_volatile() }
    }

    fn set_refcount(&self, index: usize, value: u32) {
        // Safety: see `refcount_ptr`.
        unsafe { self.refcount_ptr(index).write_volatile(value) }
    }

    /// Link `phys` at the head of the free list.
    fn push_free(&mut self, phys: u64) {
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
    fn pop_free(&mut self) -> Option<u64> {
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
    fn share(&mut self, phys: u64) -> bool {
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
    fn release(&mut self, phys: u64) -> Release {
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

    fn stats(&self) -> FrameStats {
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
struct GlobalFrames;

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
fn release_frame(phys: PhysAddr) -> Release {
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

pub use heap::HeapStats;

/// Snapshot of the kernel heap's counters; see [`HeapStats`]. The system-stats
/// syscall (issue #144) uses this for its `slab/heap usage` fields.
pub fn heap_stats() -> HeapStats {
    heap::stats()
}

/// Read the active level-4 page table through the physical-memory mapping.
///
/// # Safety
/// `offset` must be the bootloader-provided physical memory offset.
unsafe fn active_level_4_table(offset: VirtAddr) -> &'static mut PageTable {
    let (frame, _) = Cr3::read();
    let phys = frame.start_address();
    let virt = offset + phys.as_u64();
    let ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *ptr
}

/// Map one page in the active page table. Returns false on failure.
///
/// Part of the public paging surface (issue #55 moved user mappings to
/// [`map_page_in`] plus the VMA layer); kept for callers that own the active
/// table, and used by tests.
#[allow(dead_code)]
pub fn map_page(virt: VirtAddr, phys: PhysAddr, flags: PageTableFlags) -> bool {
    map_page_in(kernel_table(), virt, phys, flags)
}

/// Physical frame of the currently active (kernel) page table.
pub fn kernel_table() -> PhysAddr {
    let (frame, _) = Cr3::read();
    frame.start_address()
}

/// Create a fresh address space: a new PML4 sharing the kernel's higher-half
/// entries (indices 1..512) but with an empty user half (index 0).
pub fn new_user_table() -> Option<PhysAddr> {
    let phys = alloc_zeroed_frame()?;
    let offset = physical_offset();
    // Safety: the active table and the new frame are mapped.
    unsafe {
        let kernel = active_level_4_table(offset) as *const PageTable as *const u64;
        let table = phys_to_virt(phys).as_mut_ptr::<u64>();
        for i in 1..512 {
            core::ptr::write_volatile(table.add(i), core::ptr::read_volatile(kernel.add(i)));
        }
    }
    // The frame allocator may hand back a PML4 of a torn-down address space;
    // `register` resets any stale VMA list keyed by that physical address.
    vma::register(phys);
    Some(phys)
}

/// Map a page into a specific page table.
pub fn map_page_in(table: PhysAddr, virt: VirtAddr, phys: PhysAddr, flags: PageTableFlags) -> bool {
    let offset = physical_offset();
    let table_virt = phys_to_virt(table);
    // Safety: `table` is a PML4 frame we own.
    let level_4 = unsafe { &mut *table_virt.as_mut_ptr::<PageTable>() };
    // Safety: `offset` is the kernel's physical memory mapping offset, which
    // covers every frame `level_4` and its descendants can name.
    let mut mapper = unsafe { OffsetPageTable::new(level_4, offset) };
    let mut frames = GlobalFrames;
    let page = Page::<Size4KiB>::containing_address(virt);
    let frame = PhysFrame::containing_address(phys);
    // Safety: the virtual page is not otherwise mapped in this table.
    unsafe {
        match mapper.map_to(page, frame, flags, &mut frames) {
            Ok(flush) => {
                flush.flush();
                true
            }
            Err(err) => {
                crate::serial_println!("map_page_in {:#x} failed: {:?}", virt.as_u64(), err);
                false
            }
        }
    }
}

/// Switch the active address space.
pub fn switch_to(table: PhysAddr) {
    // Safety: `table` is a valid PML4 whose kernel entries match the current one.
    unsafe {
        Cr3::write(
            PhysFrame::containing_address(table),
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }
}

// Copy-on-write: a software bit in the (otherwise unused) page-table entry flags
// marking a shared, read-only user page. The first writer gets a private copy.
const COW_BIT: u64 = 1 << 9;
use pte::{
    ADDR as PTE_ADDR, HUGE as PTE_HUGE, NX as PTE_NX, PRESENT as PTE_PRESENT, USER as PTE_USER,
    WRITABLE as PTE_WRITABLE,
};

/// View a page table/frame as an array of raw 64-bit entries.
///
/// # Safety
/// `phys` must be mapped and large enough for the accesses made.
unsafe fn entry_table(phys: PhysAddr) -> *mut u64 {
    pte::table(phys)
}

/// Count the user data pages mapped in an address space: a diagnostic walk of
/// PML4 entry 0 (shared COW pages count once per address space). This is the
/// per-address-space accounting hook, reported when a task is reaped and
/// available to tools alongside [`frame_stats`]; keeping a running per-table
/// count is not worth the bookkeeping yet.
pub fn user_table_frame_count(table: PhysAddr) -> usize {
    // Safety: `table` is a PML4 we own.
    unsafe {
        let p4 = entry_table(table);
        let entry = *p4.add(0);
        if entry & PTE_PRESENT == 0 {
            return 0;
        }
        count_leaves(entry & PTE_ADDR, 3)
    }
}

/// Count present 4 KiB user leaves below a page table of `level`.
///
/// # Safety
/// `phys` must be a page table of `level`.
unsafe fn count_leaves(phys: u64, level: u8) -> usize {
    let mut count = 0;
    let entries = entry_table(PhysAddr::new(phys));
    for i in 0..512 {
        let entry = *entries.add(i);
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            if entry & PTE_USER != 0 {
                count += 1;
            }
        } else if entry & PTE_HUGE == 0 {
            count += count_leaves(entry & PTE_ADDR, level - 1);
        }
    }
    count
}

/// Tear down an address space's user half: shared data frames lose a reference
/// (and return to the pool at zero) and page tables are released. Returns how
/// many frames reached reference count zero.
///
/// Only PML4 entry 0 is walked; the higher-half entries are shared kernel
/// mappings that must never be freed. The PML4 frame itself is released too,
/// so the caller must ensure no other task still uses `table` (e.g. threads
/// created with `clone(CLONE_VM)`).
pub fn free_user_table(table: PhysAddr) -> usize {
    let mut released = 0;
    // Safety: `table` is a PML4 we own and are tearing down.
    unsafe {
        let p4 = entry_table(table);
        let entry = *p4.add(0);
        if entry & PTE_PRESENT != 0 {
            released += free_table(entry & PTE_ADDR, 3);
        }
    }
    if release_frame(table) == Release::Pooled {
        released += 1;
    }
    // The address space no longer exists: drop its VMA list so a recycled PML4
    // frame cannot inherit it.
    vma::forget(table);
    released
}

/// Release the page tables and data frames below a table of `level`
/// (3=PDPT .. 1=PT), then the table at `phys` itself. Returns the number of
/// frames that reached reference count zero.
///
/// # Safety
/// `phys` must be a page table of `level` that no other address space uses.
unsafe fn free_table(phys: u64, level: u8) -> usize {
    let mut released = 0;
    let entries = entry_table(PhysAddr::new(phys));
    for i in 0..512 {
        let entry = *entries.add(i);
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            // A leaf: drop one reference. Non-user leaves are kernel aliases
            // and must not be touched.
            if entry & PTE_USER != 0
                && release_frame(PhysAddr::new(entry & PTE_ADDR)) == Release::Pooled
            {
                released += 1;
            }
        } else if entry & PTE_HUGE == 0 {
            released += free_table(entry & PTE_ADDR, level - 1);
        } else {
            crate::serial_println!("mem: ignoring huge page at {:#x}", entry & PTE_ADDR);
        }
    }
    if release_frame(PhysAddr::new(phys)) == Release::Pooled {
        released += 1;
    }
    released
}

/// Share the user half (PML4 entry 0) of `parent` with a fresh address space
/// using copy-on-write: both keep the same frames with an extra reference,
/// read-only; the first writer gets a private copy (see [`cow_fault`]). Flushes
/// the parent's TLB. All user VAs live below 512 GiB, so PML4 entry 0 covers
/// them; the kernel's higher-half entries are shared by `new_user_table`.
pub fn clone_user_table(parent: PhysAddr) -> Option<PhysAddr> {
    let child = new_user_table()?;
    let mut failed = false;
    // Safety: we own both tables and every frame we touch.
    unsafe {
        let src = entry_table(parent);
        let dst = entry_table(child);
        let entry = *src.add(0);
        if entry & PTE_PRESENT != 0 {
            match cow_clone_level(entry & PTE_ADDR, 3) {
                Some(sub) => *dst.add(0) = sub | (entry & !PTE_ADDR),
                None => failed = true,
            }
        }
    }
    if failed {
        // `cow_clone_level` already released the partial subtree; drop the
        // PML4 allocated by `new_user_table`.
        free_frame(child);
    } else {
        // Fork inherits the parent's layout: the child can demand-fault and
        // `mprotect` exactly the same ranges.
        vma::clone_space(parent, child);
    }
    // Our own leaves may now be read-only (or were restored by a failed
    // clone), so drop stale writable TLB entries either way.
    switch_to(kernel_table());
    if failed {
        None
    } else {
        Some(child)
    }
}

/// Share `level` (3=PDPT .. 1=PT) into new tables, marking leaves COW in both
/// the source and the copy. On failure the partial copy is released, so a
/// failed fork leaks nothing.
///
/// # Safety
/// `src_phys` must be a page table of `level`.
unsafe fn cow_clone_level(src_phys: u64, level: u8) -> Option<u64> {
    let new_phys = alloc_zeroed_frame()?;
    let src = entry_table(PhysAddr::new(src_phys));
    let dst = entry_table(new_phys);
    for i in 0..512 {
        let entry = *src.add(i);
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            // Share the frame read-only and mark it copy-on-write in both.
            if !share_frame(PhysAddr::new(entry & PTE_ADDR)) {
                free_table(new_phys.as_u64(), level);
                return None;
            }
            *dst.add(i) = (entry & PTE_ADDR) | ((entry & !PTE_ADDR) & !PTE_WRITABLE) | COW_BIT;
            *src.add(i) = (entry & !PTE_WRITABLE) | COW_BIT;
        } else {
            match cow_clone_level(entry & PTE_ADDR, level - 1) {
                Some(sub) => *dst.add(i) = sub | (entry & !PTE_ADDR),
                None => {
                    free_table(new_phys.as_u64(), level);
                    return None;
                }
            }
        }
    }
    Some(new_phys.as_u64())
}

/// Walk `table` to the 4 KiB leaf for `va`, returning a pointer to its entry.
/// `None` means the path is absent or a huge page covers `va`; this never
/// allocates, so callers that need a page mapped go through [`map_page_in`].
///
/// # Safety
/// `table` must be a live PML4 whose lower levels are stable for the duration
/// of the returned pointer's use (no concurrent address-space teardown).
unsafe fn leaf_entry(table: PhysAddr, va: u64) -> Option<*mut u64> {
    let index = |shift: u64| ((va >> shift) & 0x1ff) as usize;
    let p4 = entry_table(table);
    let e4 = *p4.add(index(39));
    if e4 & PTE_PRESENT == 0 {
        return None;
    }
    let p3 = entry_table(PhysAddr::new(e4 & PTE_ADDR));
    let e3 = *p3.add(index(30));
    if e3 & PTE_PRESENT == 0 || e3 & PTE_HUGE != 0 {
        return None;
    }
    let p2 = entry_table(PhysAddr::new(e3 & PTE_ADDR));
    let e2 = *p2.add(index(21));
    if e2 & PTE_PRESENT == 0 || e2 & PTE_HUGE != 0 {
        return None;
    }
    let p1 = entry_table(PhysAddr::new(e2 & PTE_ADDR));
    let entry = p1.add(index(12));
    if *entry & PTE_PRESENT == 0 {
        return None;
    }
    Some(entry)
}

/// Resolve a write fault on a COW page: copy the frame and map it writable.
/// Returns true if the fault was handled (caller should resume).
pub fn cow_fault(table: PhysAddr, va: u64) -> bool {
    // Safety: we walk the given PML4, whose entries we own.
    let entry = unsafe { leaf_entry(table, va & !(FRAME_SIZE - 1)) };
    let Some(entry) = entry else {
        return false;
    };
    // Safety: `entry` was just returned by `leaf_entry` as a present leaf in
    // this same table.
    let old = unsafe { *entry };
    if old & PTE_USER == 0 || old & COW_BIT == 0 {
        return false;
    }
    let Some(frame) = alloc_zeroed_frame() else {
        return false;
    };
    copy_frame(PhysAddr::new(old & PTE_ADDR), frame);
    // Safety: `entry` is the same present leaf read above; nothing else can
    // have unmapped it in between (single-threaded fault handling).
    unsafe { *entry = frame.as_u64() | ((old & !PTE_ADDR) & !COW_BIT) | PTE_WRITABLE };
    // The page now lives privately here: release our reference to the shared
    // frame (which frees it if this was the last user).
    free_frame(PhysAddr::new(old & PTE_ADDR));
    x86_64::instructions::tlb::flush(VirtAddr::new(va));
    true
}

/// Drop `[start, end)` from `table`'s user mappings: clear each present leaf
/// and return its frame to the allocator (shared COW frames just lose one
/// reference). Page tables are left in place; [`free_user_table`] reaps them.
/// Returns the number of leaves cleared.
pub fn unmap_range(table: PhysAddr, start: u64, end: u64) -> usize {
    let mut cleared = 0;
    let mut va = start & !(FRAME_SIZE - 1);
    while va < end {
        // Safety: `table` is a live address space and we own its entries.
        if let Some(entry) = unsafe { leaf_entry(table, va) } {
            // Safety: `entry` was just returned as a present leaf in this table.
            let value = unsafe { *entry };
            if value & PTE_USER != 0 {
                // Safety: same `entry`, still valid; nothing else can have
                // unmapped it in between (single-threaded teardown).
                unsafe { *entry = 0 };
                free_frame(PhysAddr::new(value & PTE_ADDR));
                cleared += 1;
                x86_64::instructions::tlb::flush(VirtAddr::new(va));
            }
        }
        va += FRAME_SIZE;
    }
    cleared
}

/// Move the mapping of `old_va` to `new_va` without copying the frame: the
/// physical frame, COW bit and protection move with the PTE. Used by `mremap`
/// to relocate a mapping (`new_va` must be unmapped). `Ok(false)` means the
/// source page was not resident; `Err(())` means the destination page tables
/// could not be allocated (the source was restored).
pub fn remap_page(table: PhysAddr, old_va: u64, new_va: u64) -> Result<bool, ()> {
    let old_page = old_va & !(FRAME_SIZE - 1);
    let new_page = new_va & !(FRAME_SIZE - 1);
    // Safety: `table` is a live address space and we own its entries.
    let Some(old_entry) = (unsafe { leaf_entry(table, old_page) }) else {
        return Ok(false);
    };
    // Safety: `old_entry` was just returned as a present leaf in this table.
    let value = unsafe { *old_entry };
    if value & PTE_USER == 0 {
        return Ok(false);
    }
    // Safety: same entry, still valid; nothing else can have unmapped it in
    // between (single-threaded remap).
    unsafe { *old_entry = 0 };
    x86_64::instructions::tlb::flush(VirtAddr::new(old_page));
    let flags = PageTableFlags::from_bits_truncate(value & !PTE_ADDR);
    if !map_page_in(
        table,
        VirtAddr::new(new_page),
        PhysAddr::new(value & PTE_ADDR),
        flags,
    ) {
        // Safety: the entry is still ours and was cleared just above.
        unsafe { *old_entry = value };
        x86_64::instructions::tlb::flush(VirtAddr::new(old_page));
        return Err(());
    }
    Ok(true)
}

/// Apply `prot` to the present user pages of `[start, end)` in `table`.
/// A COW page is privatized first: its protection is per-address-space, so it
/// must not keep sharing a frame after `mprotect`. Returns false when
/// privatizing needed memory and none was available (pages updated before the
/// failure keep their new protection).
pub fn protect_range(table: PhysAddr, start: u64, end: u64, prot: vma::Prot) -> bool {
    let mut va = start & !(FRAME_SIZE - 1);
    while va < end {
        // Safety: `table` is a live address space and we own its entries.
        if let Some(entry) = unsafe { leaf_entry(table, va) } {
            // Safety: `entry` was just returned as a present leaf in this table.
            let old = unsafe { *entry };
            if old & PTE_USER != 0 {
                let new = if old & COW_BIT != 0 {
                    // The page is shared read-only: copy it before changing the
                    // protection, so this address space gets a private frame.
                    let Some(frame) = alloc_zeroed_frame() else {
                        return false;
                    };
                    copy_frame(PhysAddr::new(old & PTE_ADDR), frame);
                    free_frame(PhysAddr::new(old & PTE_ADDR));
                    frame.as_u64() | (old & !PTE_ADDR & !(PTE_WRITABLE | COW_BIT | PTE_NX))
                } else {
                    old
                };
                // Safety: same `entry`, still valid; nothing else can have
                // unmapped it in between (single-threaded `mprotect`).
                unsafe { *entry = apply_prot(new, prot) };
                x86_64::instructions::tlb::flush(VirtAddr::new(va));
            }
        }
        va += FRAME_SIZE;
    }
    true
}

/// Add the flags `prot` implies to an already present PTE value.
fn apply_prot(mut entry: u64, prot: vma::Prot) -> u64 {
    entry &= !(PTE_WRITABLE | PTE_NX);
    if prot.has_write() {
        entry |= PTE_WRITABLE;
    }
    if !prot.has_exec() {
        entry |= PTE_NX;
    }
    entry
}

/// Copy one 4 KiB frame through the physical-memory mapping.
fn copy_frame(source: PhysAddr, destination: PhysAddr) {
    // Safety: both frames are mapped and exclusively owned by the caller.
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(source).as_ptr::<u8>(),
            phys_to_virt(destination).as_mut_ptr::<u8>(),
            4096,
        );
    }
}

/// Page-table flags for a VMA protection value. Absent `EXEC` maps as NX
/// (`init` enables EFER.NXE), so stacks, heaps and anonymous memory default to
/// non-executable.
pub fn prot_flags(prot: vma::Prot) -> PageTableFlags {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot.has_write() {
        flags |= PageTableFlags::WRITABLE;
    }
    if !prot.has_exec() {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    flags
}

/// Resolve a not-present page fault by materializing a zeroed page for an
/// `Anon`/`Heap` VMA. Returns true if the fault was handled.
///
/// Only access the VMA permits is granted: a write fault in a read-only range
/// (or any access to `PROT_NONE`) stays unresolved and falls through to the
/// fatal path, where a future signal would be delivered. `File`/`Stack` VMAs
/// are mapped eagerly and never demand-fault.
pub fn demand_fault(table: PhysAddr, va: u64, error: PageFaultErrorCode) -> bool {
    if error.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        return false; // present but forbidden: not a missing page
    }
    let Some(vma) = vma::find(table, va) else {
        return false;
    };
    if !matches!(vma.kind, vma::Kind::Anon | vma::Kind::Heap) {
        return false;
    }
    if !(vma.prot.has_read() || vma.prot.has_exec()) {
        return false;
    }
    let Some(frame) = alloc_zeroed_frame() else {
        return false;
    };
    let page = VirtAddr::new(va & !(FRAME_SIZE - 1));
    if !map_page_in(table, page, frame, prot_flags(vma.prot)) {
        free_frame(frame);
        return false;
    }
    true
}

/// Per-address-space accounting: `(vsz_bytes, resident_pages)`.
///
/// VSZ is the summed VMA length (what the process has reserved); resident
/// pages are the present 4 KiB user leaves (shared COW pages count once per
/// address space). This is the hook tools/tests use to report VSZ/RSS.
#[allow(dead_code)]
pub fn vma_stats(table: PhysAddr) -> (u64, usize) {
    let vsz = vma::list(table).iter().map(|vma| vma.len()).sum();
    (vsz, user_table_frame_count(table))
}

/// Initialise frame allocation and the kernel heap.
pub fn init(boot_info: &'static mut BootInfo) {
    let offset = match boot_info.physical_memory_offset {
        Optional::Some(offset) => VirtAddr::new(offset),
        Optional::None => panic!("bootloader did not map physical memory"),
    };
    PHYS_OFFSET.store(offset.as_u64(), Ordering::Relaxed);

    // Turn on EFER.NXE before building any VMA-derived mapping: `prot_flags`
    // sets the NX bit for non-executable pages, and with NXE off that bit is
    // reserved and would fault on every access. Long mode on every CPU we
    // target (and QEMU) supports no-execute.
    // Safety: runs once at boot before any other CPU state depends on EFER.
    unsafe {
        use x86_64::registers::model_specific::{Efer, EferFlags};
        Efer::update(|flags| flags.insert(EferFlags::NO_EXECUTE_ENABLE));
    }

    // Gather the usable regions, clamping away the low megabyte that holds
    // the kernel and the bootloader's metadata.
    let mut starts = [0u64; MAX_REGIONS];
    let mut ends = [0u64; MAX_REGIONS];
    let mut count = 0;
    let mut highest = LOWEST_FRAME;
    for region in boot_info
        .memory_regions
        .iter()
        .filter(|r| r.kind == MemoryRegionKind::Usable && r.end > LOWEST_FRAME)
    {
        if count == MAX_REGIONS {
            break;
        }
        let start = region.start.max(LOWEST_FRAME);
        if start + FRAME_SIZE > region.end {
            continue;
        }
        starts[count] = start;
        ends[count] = region.end;
        count += 1;
        highest = highest.max(region.end);
    }

    // Carve the refcount table out of the first region with room: one `u32`
    // per frame up to the highest usable address.
    let table_entries = (highest / FRAME_SIZE) as usize;
    let table_bytes = table_entries * core::mem::size_of::<u32>();
    let table_frames = table_bytes.div_ceil(FRAME_SIZE as usize);
    // Boot has no caller to propagate a placement failure to: a genuine kstop.
    let table_phys = place_table(&starts, &ends, count, table_frames)
        .unwrap_or_else(|| kstop(KError::OutOfMemory, "no room for the frame refcount table"));

    let mut frames = Frames {
        starts,
        ends,
        count,
        refcounts: table_phys,
        free_head: FREE_LIST_END,
        untouched: untouched::Untouched::new(&starts),
        total: 0,
        allocated: 0,
        freed: 0,
        reserved: table_frames,
        double_frees: 0,
        invalid_frees: 0,
    };
    // Zero the table and reserve its frames; `RESERVED` entries keep them out
    // of circulation. Safety: the table is a reserved contiguous run.
    unsafe {
        core::ptr::write_bytes(
            phys_to_virt(PhysAddr::new(table_phys)).as_mut_ptr::<u8>(),
            0,
            table_bytes,
        );
    }
    for i in 0..table_frames as u64 {
        frames.set_refcount(Frames::index(table_phys + i * FRAME_SIZE), RESERVED);
    }
    // Frames are not linked yet: `untouched` hands them out on demand.
    let in_regions: usize = (0..count)
        .map(|i| untouched::Untouched::frames_in(starts[i], ends[i]))
        .sum();
    frames.total = in_regions - table_frames;
    let boot = frames.stats();
    *FRAMES.lock() = Some(frames);
    // The slab allocator needs only frames and the physical-memory mapping, so
    // it is usable from this point on, before the heap below is mapped. Its
    // oversized fallback is the only path that needs the heap.
    serial_println!(
        "mem: {} frames usable ({} MiB), {} reserved, {} free",
        boot.total,
        boot.total as u64 * FRAME_SIZE / (1024 * 1024),
        boot.reserved,
        boot.free
    );

    // Map the kernel heap.
    // Safety: `offset` is the kernel's physical memory mapping offset, which
    // covers every frame the active level-4 table and its descendants name.
    let mut mapper = unsafe { OffsetPageTable::new(active_level_4_table(offset), offset) };
    let mut frames = GlobalFrames;
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(HEAP_START));
    let end_page = Page::containing_address(VirtAddr::new(HEAP_START + HEAP_SIZE - 1));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    for page in Page::range_inclusive(start_page, end_page) {
        // As above: no caller exists yet to receive a Result.
        let frame = frames
            .allocate_frame()
            .unwrap_or_else(|| kstop(KError::OutOfMemory, "out of frames mapping the heap"));
        // Safety: the heap range is reserved and not otherwise mapped.
        unsafe {
            match mapper.map_to(page, frame, flags, &mut frames) {
                Ok(flush) => flush.flush(),
                Err(_) => kstop(KError::Io, "failed to map heap page"),
            }
        }
    }
    // Safety: the range was just mapped writable and is otherwise unused.
    unsafe { heap::init(HEAP_START as usize, HEAP_SIZE as usize) };
}

/// Find the first region with room for `frame_count` contiguous frames and
/// return the frame-aligned physical address for the refcount table.
fn place_table(
    starts: &[u64; MAX_REGIONS],
    ends: &[u64; MAX_REGIONS],
    count: usize,
    frame_count: usize,
) -> Option<u64> {
    let bytes = frame_count as u64 * FRAME_SIZE;
    for i in 0..count {
        let start = (starts[i] + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
        if start + bytes <= ends[i] {
            return Some(start);
        }
    }
    None
}
