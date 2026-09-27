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

const LOWEST_FRAME: u64 = 0x10_0000;
/// Maximum usable memory regions we track (no heap needed to bootstrap).
const MAX_REGIONS: usize = 32;

static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

struct Frames {
    starts: [u64; MAX_REGIONS],
    ends: [u64; MAX_REGIONS],
    count: usize,
    index: usize,
    cursor: u64,
    allocated: usize,
}

static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// Adapter so `map_to` can pull frames from the global allocator.
struct GlobalFrames;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrames {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        alloc_frame().map(PhysFrame::containing_address)
    }
}

/// Allocate one 4 KiB frame, returning its physical address.
pub fn alloc_frame() -> Option<PhysAddr> {
    let mut guard = FRAMES.lock();
    let frames = guard.as_mut()?;
    while frames.index < frames.count {
        let start = frames.starts[frames.index];
        let end = frames.ends[frames.index];
        let candidate = frames.cursor.max(start);
        let aligned = (candidate + 0xFFF) & !0xFFF;
        if aligned + 4096 <= end {
            frames.cursor = aligned + 4096;
            frames.allocated += 1;
            return Some(PhysAddr::new(aligned));
        }
        frames.index += 1;
        frames.cursor = 0;
    }
    None
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

/// Share the user half (PML4 entry 0) of `parent` with a fresh address space
/// using copy-on-write: both keep the same frames, read-only; the first writer
/// gets a private copy (see [`cow_fault`]). Flushes the parent's TLB. All user
/// VAs live below 512 GiB, so PML4 entry 0 covers them; the kernel's higher-half
/// entries are shared by `new_user_table`.
pub fn clone_user_table(parent: PhysAddr) -> Option<PhysAddr> {
    let child = new_user_table()?;
    // Safety: we own both tables and every frame we touch.
    unsafe {
        let src = entry_table(parent);
        let dst = entry_table(child);
        let entry = *src.add(0);
        if entry & PTE_PRESENT != 0 {
            let sub = cow_clone_level(entry & PTE_ADDR, 3)?;
            *dst.add(0) = sub | (entry & !PTE_ADDR);
        }
    }
    // Our own leaves are now read-only; drop stale writable TLB entries.
    switch_to(kernel_table());
    Some(child)
}

/// Share `level` (3=PDPT .. 1=PT) into new tables, marking leaves COW in both
/// the source and the copy.
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
            *dst.add(i) = (entry & PTE_ADDR) | ((entry & !PTE_ADDR) & !PTE_WRITABLE) | COW_BIT;
            *src.add(i) = (entry & !PTE_WRITABLE) | COW_BIT;
        } else {
            let sub = cow_clone_level(entry & PTE_ADDR, level - 1)?;
            *dst.add(i) = sub | (entry & !PTE_ADDR);
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

    let mut starts = [0u64; MAX_REGIONS];
    let mut ends = [0u64; MAX_REGIONS];
    let mut count = 0;
    for region in boot_info
        .memory_regions
        .iter()
        .filter(|r| r.kind == MemoryRegionKind::Usable && r.end > LOWEST_FRAME)
    {
        if count == MAX_REGIONS {
            break;
        }
        let start = region.start.max(LOWEST_FRAME);
        if start + 4096 > region.end {
            continue;
        }
        starts[count] = start;
        ends[count] = region.end;
        count += 1;
    }
    *FRAMES.lock() = Some(Frames {
        starts,
        ends,
        count,
        index: 0,
        cursor: LOWEST_FRAME,
        allocated: 0,
    });

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
