//! Memory management: physical frames, kernel paging, and the heap.

mod frame;
mod heap;

pub use frame::BumpFrameAllocator;

use bootloader_api::info::Optional;
use bootloader_api::BootInfo;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, Size4KiB,
};
use x86_64::VirtAddr;
/// Virtual base of the kernel heap.
pub const HEAP_START: u64 = 0x_4444_4444_0000;
/// Size of the kernel heap (1 MiB).
pub const HEAP_SIZE: u64 = 1024 * 1024;

/// Summary of memory initialisation, for logging.
#[derive(Clone, Copy, Debug)]
pub struct MemStats {
    pub heap_size: usize,
    pub frames_allocated: usize,
    pub frames_total: usize,
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

/// Initialise paging access and the kernel heap.
pub fn init(boot_info: &'static mut BootInfo) -> MemStats {
    let offset = match boot_info.physical_memory_offset {
        Optional::Some(offset) => VirtAddr::new(offset),
        Optional::None => panic!("bootloader did not map physical memory"),
    };

    // Safety: the bootloader mapped all physical memory at `offset`.
    let mut mapper = unsafe { OffsetPageTable::new(active_level_4_table(offset), offset) };
    let mut frames = BumpFrameAllocator::new(&boot_info.memory_regions);

    let start_page = Page::<Size4KiB>::containing_address(VirtAddr::new(HEAP_START));
    let end_page = Page::containing_address(VirtAddr::new(HEAP_START + HEAP_SIZE - 1));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    for page in Page::range_inclusive(start_page, end_page) {
        let frame = frames
            .allocate_frame()
            .expect("out of frames mapping the heap");
        // Safety: the virtual range is reserved for the heap and not mapped yet.
        unsafe {
            mapper
                .map_to(page, frame, flags, &mut frames)
                .expect("failed to map heap page")
                .flush();
        }
    }

    // Safety: the whole range was just mapped writable and is otherwise unused.
    unsafe { heap::init(HEAP_START as usize, HEAP_SIZE as usize) };

    MemStats {
        heap_size: HEAP_SIZE as usize,
        frames_allocated: frames.allocated(),
        frames_total: frames.total(),
    }
}
