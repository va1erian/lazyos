//! Memory management: physical frames, kernel paging, the slab allocator, and
//! the heap.

mod cow;
mod frames;
mod heap;
pub mod mmio;
pub mod pte;
mod reclaim;
pub use reclaim::reclaim_empty_tables;
pub mod slab;
mod table_guard;
pub mod untouched;
mod uspace;
pub mod vma;
pub use cow::clone_user_table;
pub use frames::*;
pub use table_guard::UserTableGuard;
pub use uspace::*;

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

/// Maximum usable memory regions we track (no heap needed to bootstrap).
pub const MAX_REGIONS: usize = 32;

#[cfg(lazyos_tests)]
pub use heap::harness as heap_harness;
pub use heap::{locked as heap_locked, HeapStats};

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
