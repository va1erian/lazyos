//! Memory management: physical frames, kernel paging, the slab allocator, and
//! the heap.

mod cow;
pub mod dma;
mod frames;
mod heap;
mod layout;
pub mod mmio;
pub mod pte;
mod reclaim;
mod regions;
pub use reclaim::reclaim_empty_tables;
pub mod fbwindow;
pub mod slab;
mod table_guard;
pub mod untouched;
mod uspace;
pub mod vma;
pub mod wc;
pub use cow::clone_user_table;
pub use dma::{dma_alloc, dma_stats};
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

pub use heap::HEAP_START;
pub use layout::*;
#[cfg(lazyos_tests)]
pub use regions::PHYS_LIMIT;
pub use regions::{Regions, MAX_REGIONS};

/// Physical address of the kernel's (boot) PML4, recorded by [`init`]: the
/// table heap growth maps into, whatever address space is active.
static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);
/// Usable RAM in bytes, as the memory map reported it.
static USABLE_RAM: AtomicU64 = AtomicU64::new(0);

/// Usable RAM in bytes (the merged usable regions of the memory map).
pub fn usable_ram() -> u64 {
    USABLE_RAM.load(Ordering::Relaxed)
}

#[cfg(lazyos_tests)]
pub use heap::harness as heap_harness;
#[cfg(lazyos_tests)]
pub use heap::small_live as heap_small_live;
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

    // Gather the usable regions (merged, sorted, clamped away from the low
    // megabyte that holds the kernel and the bootloader's metadata).
    let mut regions = Regions::gather(
        boot_info
            .memory_regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| (r.start, r.end)),
    );
    // Carve the refcount table out of the first region with room: one `u32`
    // per frame up to the highest usable address. When nothing can hold it,
    // the highest region (what makes it big) is given up and placement is
    // retried, so a map with one absurd range still boots with the rest.
    let (table_phys, table_frames) = loop {
        let table_entries = (regions.highest().max(LOWEST_FRAME) / FRAME_SIZE) as usize;
        let table_bytes = table_entries * core::mem::size_of::<u32>();
        let table_frames = table_bytes.div_ceil(FRAME_SIZE as usize);
        if let Some(phys) = place_table(&regions.starts, &regions.ends, regions.count, table_frames)
        {
            break (phys, table_frames);
        }
        if regions.count == 0 {
            // Boot has no caller to propagate a placement failure to.
            kstop(KError::OutOfMemory, "no room for the frame refcount table");
        }
        regions.drop_highest();
    };
    if regions.dropped > 0 {
        serial_println!(
            "mem: {} KiB of usable RAM left out (more than {} disjoint regions, or out of reach)",
            regions.dropped / 1024,
            MAX_REGIONS
        );
    }
    let (starts, ends, count) = (regions.starts, regions.ends, regions.count);
    let highest = regions.highest().max(LOWEST_FRAME);
    let ram = regions.bytes();
    USABLE_RAM.store(ram, Ordering::Relaxed);
    let table_bytes = table_frames * FRAME_SIZE as usize;

    let mut frames = Frames {
        starts,
        ends,
        count,
        refcounts: table_phys,
        free_head: FREE_LIST_END,
        untouched: untouched::Untouched::new(&starts),
        pool: dma::DmaPool::empty(),
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
    // Reserve the DMA pool once, from the memory map (issue #241): its frames
    // stay in the refcount table but are marked `RESERVED` while free, so the
    // general allocator (which skips `RESERVED` in `pop_free`) never hands them
    // out, and they are excluded from `total`/`free` like the metadata table.
    let mut pool_pages = 0usize;
    let pool_bytes = crate::limits::dma_pool_bytes(ram);
    if let Some((base, pages)) = dma::choose_pool(&regions, pool_bytes, table_phys, table_frames) {
        frames.pool.reserve(base, pages);
        for page in 0..u64::from(pages) {
            frames.set_refcount(Frames::index(base + page * FRAME_SIZE), RESERVED);
        }
        pool_pages = pages as usize;
    }
    frames.total = in_regions - table_frames - pool_pages;
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

    serial_println!(
        "mem: {} MiB in {} regions, highest {:#x}, DMA pool {} KiB",
        ram >> 20,
        count,
        highest,
        pool_pages * FRAME_SIZE as usize / 1024
    );

    let (pml4, _) = Cr3::read();
    KERNEL_PML4.store(pml4.start_address().as_u64(), Ordering::Relaxed);
    layout::check_kernel_table(pml4.start_address());

    // Map the initial kernel heap; it grows on demand from here.
    let initial = crate::limits::heap_initial_bytes(ram);
    if map_kernel_range(HEAP_START, initial / FRAME_SIZE) * FRAME_SIZE != initial {
        kstop(KError::OutOfMemory, "out of frames mapping the heap");
    }
    // Safety: the range was just mapped writable and is otherwise unused.
    unsafe { heap::init(initial) };
}

/// Map `pages` fresh frames at `start` in the kernel's own table, writable
/// and no-execute, for kernel-half ranges every address space shares (the
/// heap). Stops at the first failure and returns how many pages it mapped.
/// The caller owns `[start, start + pages * 4 KiB)` and nothing else maps it.
pub(crate) fn map_kernel_range(start: u64, pages: u64) -> u64 {
    let table = PhysAddr::new(KERNEL_PML4.load(Ordering::Relaxed));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    for index in 0..pages {
        let Some(frame) = alloc_frame() else {
            return index;
        };
        let va = VirtAddr::new(start + index * FRAME_SIZE);
        if !map_page_in(table, va, frame, flags) {
            free_frame(frame);
            return index;
        }
    }
    pages
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
