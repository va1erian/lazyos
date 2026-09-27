//! Memory management: physical frames, kernel paging, and the heap.

mod heap;

use bootloader_api::info::{MemoryRegionKind, Optional};
use bootloader_api::BootInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Virtual base of the kernel heap.
pub const HEAP_START: u64 = 0x_4444_4444_0000;
/// Size of the kernel heap (16 MiB): enough for a full-screen RGBA pixmap.
pub const HEAP_SIZE: u64 = 16 * 1024 * 1024;

/// Never hand out frames below this: the bootloader loads the kernel and its
/// metadata in the first megabyte.
const LOWEST_FRAME: u64 = 0x10_0000;
/// Maximum usable memory regions we track (no heap needed to bootstrap).
const MAX_REGIONS: usize = 32;
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
/// out of the first usable region, marks those frames [`RESERVED`], and links
/// every other usable frame into the free list. Both are reached through the
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

    /// Unlink and return the head of the free list.
    fn pop_free(&mut self) -> Option<u64> {
        if self.free_head == FREE_LIST_END {
            return None;
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
    Some(phys)
}

/// Map a page into a specific page table.
pub fn map_page_in(table: PhysAddr, virt: VirtAddr, phys: PhysAddr, flags: PageTableFlags) -> bool {
    let offset = physical_offset();
    let table_virt = phys_to_virt(table);
    // Safety: `table` is a PML4 frame we own.
    let level_4 = unsafe { &mut *table_virt.as_mut_ptr::<PageTable>() };
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
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_USER: u64 = 1 << 2;
const PTE_HUGE: u64 = 1 << 7;
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// View a page table/frame as an array of raw 64-bit entries.
///
/// # Safety
/// `phys` must be mapped and large enough for the accesses made.
unsafe fn entry_table(phys: PhysAddr) -> *mut u64 {
    phys_to_virt(phys).as_mut_ptr::<u64>()
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

/// Resolve a write fault on a COW page: copy the frame and map it writable.
/// Returns true if the fault was handled (caller should resume).
pub fn cow_fault(table: PhysAddr, va: u64) -> bool {
    let index = |shift: u64| ((va >> shift) & 0x1ff) as usize;
    // Safety: we walk the given PML4, whose entries we own.
    unsafe {
        let p4 = entry_table(table);
        let e4 = *p4.add(index(39));
        if e4 & PTE_PRESENT == 0 {
            return false;
        }
        let p3 = entry_table(PhysAddr::new(e4 & PTE_ADDR));
        let e3 = *p3.add(index(30));
        if e3 & PTE_PRESENT == 0 || e3 & PTE_HUGE != 0 {
            return false;
        }
        let p2 = entry_table(PhysAddr::new(e3 & PTE_ADDR));
        let e2 = *p2.add(index(21));
        if e2 & PTE_PRESENT == 0 || e2 & PTE_HUGE != 0 {
            return false;
        }
        let p1 = entry_table(PhysAddr::new(e2 & PTE_ADDR));
        let e1 = *p1.add(index(12));
        if e1 & PTE_PRESENT == 0 || e1 & PTE_USER == 0 || e1 & COW_BIT == 0 {
            return false;
        }
        let Some(frame) = alloc_zeroed_frame() else {
            return false;
        };
        core::ptr::copy_nonoverlapping(
            phys_to_virt(PhysAddr::new(e1 & PTE_ADDR)).as_ptr::<u8>(),
            phys_to_virt(frame).as_mut_ptr::<u8>(),
            4096,
        );
        *p1.add(index(12)) = frame.as_u64() | ((e1 & !PTE_ADDR) & !COW_BIT) | PTE_WRITABLE;
        // The page now lives privately here: release our reference to the
        // shared frame (which frees it if this was the last user).
        free_frame(PhysAddr::new(e1 & PTE_ADDR));
    }
    x86_64::instructions::tlb::flush(VirtAddr::new(va));
    true
}

/// Initialise frame allocation and the kernel heap.
pub fn init(boot_info: &'static mut BootInfo) {
    let offset = match boot_info.physical_memory_offset {
        Optional::Some(offset) => VirtAddr::new(offset),
        Optional::None => panic!("bootloader did not map physical memory"),
    };
    PHYS_OFFSET.store(offset.as_u64(), Ordering::Relaxed);

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
    let table_phys = place_table(&starts, &ends, count, table_frames)
        .expect("mem: no room for the frame refcount table");

    let mut frames = Frames {
        starts,
        ends,
        count,
        refcounts: table_phys,
        free_head: FREE_LIST_END,
        total: 0,
        allocated: 0,
        freed: 0,
        reserved: table_frames,
        double_frees: 0,
        invalid_frees: 0,
    };
    // Zero the table, reserve its frames, and link everything else into the
    // free list. The table is initialized before the first frame is pushed,
    // and `RESERVED` entries keep the table's own frames out of the list.
    // Safety: the table is a reserved contiguous run in usable memory.
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
    for i in 0..count {
        let mut phys = starts[i];
        while phys + FRAME_SIZE <= ends[i] {
            if frames.refcount(Frames::index(phys)) != RESERVED {
                frames.push_free(phys);
                frames.total += 1;
            }
            phys += FRAME_SIZE;
        }
    }
    let boot = frames.stats();
    *FRAMES.lock() = Some(frames);
    serial_println!(
        "mem: {} frames usable ({} MiB), {} reserved, {} free",
        boot.total,
        boot.total as u64 * FRAME_SIZE / (1024 * 1024),
        boot.reserved,
        boot.free
    );

    // Map the kernel heap.
    let mut mapper = unsafe { OffsetPageTable::new(active_level_4_table(offset), offset) };
    let mut frames = GlobalFrames;
    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(HEAP_START));
    let end_page = Page::containing_address(VirtAddr::new(HEAP_START + HEAP_SIZE - 1));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    for page in Page::range_inclusive(start_page, end_page) {
        let frame = frames
            .allocate_frame()
            .expect("out of frames mapping the heap");
        // Safety: the heap range is reserved and not otherwise mapped.
        unsafe {
            mapper
                .map_to(page, frame, flags, &mut frames)
                .expect("failed to map heap page")
                .flush();
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
