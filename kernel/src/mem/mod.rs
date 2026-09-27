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

/// Deep-copy the user half (PML4 entry 0) of `parent` into a fresh address
/// space, used by `fork`. Returns the new PML4. All user VAs live below 512 GiB,
/// so a single PML4 entry covers them; the kernel's higher-half entries are
/// shared by `new_user_table`.
pub fn clone_user_table(parent: PhysAddr) -> Option<PhysAddr> {
    let child = new_user_table()?;
    let offset = physical_offset();
    // Safety: both tables and every cloned frame are memory we own.
    unsafe {
        let src = table_mut(offset, parent);
        let dst = table_mut(offset, child);
        if !src[0].is_unused() {
            let flags = src[0].flags();
            let sub = clone_level(offset, src[0].addr(), 3)?;
            dst[0].set_addr(PhysAddr::new(sub), flags);
        }
    }
    Some(child)
}

/// Borrow a page table frame we own.
///
/// # Safety
/// `phys` must be a page-table frame and `offset` the physical-memory offset.
unsafe fn table_mut(_offset: VirtAddr, phys: PhysAddr) -> &'static mut PageTable {
    &mut *phys_to_virt(phys).as_mut_ptr::<PageTable>()
}

/// Recursively copy `level` (4=PML4 .. 1=PT); at the leaf, copy the 4 KiB frame.
///
/// # Safety
/// `src_phys` must be a page table of `level`.
unsafe fn clone_level(offset: VirtAddr, src_phys: PhysAddr, level: u8) -> Option<u64> {
    let new_phys = alloc_zeroed_frame()?;
    let src = table_mut(offset, src_phys);
    let dst = table_mut(offset, new_phys);
    for i in 0..512 {
        if src[i].is_unused() {
            continue;
        }
        let flags = src[i].flags();
        if level == 1 {
            // Leaf: allocate a new frame and copy the page contents.
            let frame = alloc_zeroed_frame()?;
            core::ptr::copy_nonoverlapping(
                phys_to_virt(src[i].addr()).as_ptr::<u8>(),
                phys_to_virt(frame).as_mut_ptr::<u8>(),
                4096,
            );
            dst[i].set_addr(frame, flags | PageTableFlags::WRITABLE);
        } else {
            let sub = clone_level(offset, src[i].addr(), level - 1)?;
            dst[i].set_addr(PhysAddr::new(sub), flags);
        }
    }
    Some(new_phys.as_u64())
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
