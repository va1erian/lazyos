//! Device MMIO mappings for user drivers (issue #240).
//!
//! A device BAR is not RAM: its frames are not in the frame allocator, so the
//! ordinary teardown paths ([`super::unmap_range`], [`super::free_user_table`],
//! [`super::clone_user_table`]) must never treat a leaf that names one as a
//! frame to free or share. Every MMIO leaf therefore carries [`pte::MMIO`], a
//! software-defined page-table bit, and those paths skip it. Mappings are
//! created uncached (`PCD|PWT`) so a register write is never reordered or
//! combined in the cache.

use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use super::pte;
use super::{leaf_entry, map_page_in, FRAMES, FRAME_SIZE};

/// Page-table flags for a user MMIO page: present, user, writable, no-execute,
/// uncached, and tagged so teardown never frees the frame.
fn mmio_flags() -> PageTableFlags {
    PageTableFlags::PRESENT
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH
        | PageTableFlags::BIT_10
}

/// Whether any byte of `[start, end)` lies in usable RAM. A BAR that overlaps
/// RAM (a misprogrammed device) must never be handed to userspace: mapping it
/// would give a driver the kernel's memory.
pub fn overlaps_ram(start: u64, end: u64) -> bool {
    match FRAMES.lock().as_ref() {
        Some(frames) => (0..frames.count).any(|i| start < frames.ends[i] && end > frames.starts[i]),
        // Before the allocator exists nothing can be checked: fail closed.
        None => true,
    }
}

/// Map `pages` pages of device memory at `phys` to `va` in `table`. On failure
/// every page mapped so far is removed again, so the range is left clean.
pub fn map_mmio(table: PhysAddr, va: u64, phys: u64, pages: u64) -> bool {
    for page in 0..pages {
        let offset = page * FRAME_SIZE;
        if !map_page_in(
            table,
            VirtAddr::new(va + offset),
            PhysAddr::new(phys + offset),
            mmio_flags(),
        ) {
            unmap_mmio(table, va, phys, page);
            return false;
        }
    }
    true
}

/// Remove the MMIO mapping of `pages` pages at `va`, without freeing any frame.
///
/// Only a leaf that carries [`pte::MMIO`] *and* still names the expected device
/// frame is cleared: if `table` was recycled for another address space since the
/// mapping was made, a stray leaf at the same address is left alone. Returns
/// how many leaves were cleared.
pub fn unmap_mmio(table: PhysAddr, va: u64, phys: u64, pages: u64) -> u64 {
    let mut cleared = 0;
    for page in 0..pages {
        let at = va + page * FRAME_SIZE;
        let expected = phys + page * FRAME_SIZE;
        // SAFETY: `leaf_entry` only walks present, non-huge levels of `table`
        // and never allocates; a stale `table` is the caller's contract (the
        // frame identity check below keeps a recycled one harmless).
        if let Some(entry) = unsafe { leaf_entry(table, at) } {
            // SAFETY: `entry` was just returned as a present leaf of `table`.
            let value = unsafe { entry.read_volatile() };
            if value & pte::MMIO != 0 && value & pte::ADDR == expected {
                // SAFETY: same entry; the single CPU cannot race this store.
                unsafe { entry.write_volatile(0) };
                x86_64::instructions::tlb::flush(VirtAddr::new(at));
                cleared += 1;
            }
        }
    }
    cleared
}

/// One past the highest physical address this CPU can address (`MAXPHYADDR`
/// from CPUID leaf 0x8000_0008, 36 bits when the leaf is missing). A 64-bit
/// BAR is hostile input: a base above this would set reserved page-table bits
/// (and above 52 bits `PhysAddr::new` panics), so `map_bar` refuses it.
pub fn phys_limit() -> u64 {
    let bits = if core::arch::x86_64::__cpuid(0x8000_0000).eax >= 0x8000_0008 {
        core::arch::x86_64::__cpuid(0x8000_0008).eax & 0xFF
    } else {
        36
    };
    1u64 << bits.clamp(32, 52)
}

/// The leaf entry that maps kernel virtual address `va` in the active
/// (kernel) table, and the size of the page it maps. Walks through 1 GiB and
/// 2 MiB leaves, which is how the bootloader maps physical memory.
fn kernel_leaf(va: u64) -> Option<(*mut u64, u64)> {
    let mut table = super::kernel_table();
    for (level, shift) in [39u64, 30, 21, 12].into_iter().enumerate() {
        let index = ((va >> shift) & 0x1ff) as usize;
        // SAFETY: `table` is the active PML4 or the target of a present,
        // non-leaf entry from the previous level, so it is a live page table
        // reached through the physical map, and `index` < 512.
        let entry = unsafe { pte::table(table).add(index) };
        // SAFETY: as above; a volatile read of one entry.
        let value = unsafe { entry.read_volatile() };
        if value & pte::PRESENT == 0 {
            return None;
        }
        if shift == 12 || (level > 0 && value & pte::HUGE != 0) {
            return Some((entry, 1 << shift));
        }
        table = PhysAddr::new(value & pte::ADDR);
    }
    None
}

/// Whether every byte of physical `[phys, phys + len)` is reachable through
/// the kernel's physical-memory mapping. Firmware tables can name any
/// address; this is the check before reading one (the bootloader maps the
/// memory map's extent and at least the first 4 GiB, nothing beyond).
pub fn phys_mapped(phys: u64, len: u64) -> bool {
    let offset = super::physical_offset().as_u64();
    let Some(last) = phys.checked_add(len.max(1) - 1) else {
        return false;
    };
    let (Some(mut va), Some(end)) = (offset.checked_add(phys), offset.checked_add(last)) else {
        return false;
    };
    // Each step moves to the next page of whatever size maps `va`; callers
    // check table-sized ranges, so the bound is never reached in practice.
    for _ in 0..4096u32 {
        let Some((_, size)) = kernel_leaf(va) else {
            return false;
        };
        let next = (va & !(size - 1)).saturating_add(size);
        if next > end {
            return true;
        }
        va = next;
    }
    false
}

/// Make the physical-map page that holds `phys` strong uncacheable
/// (`PCD|PWT`: PAT entry 3 in the power-on PAT), as the local APIC and HPET
/// registers require; the bootloader maps them write-back with the rest of
/// the first 4 GiB. The physical map is shared by every address space, so
/// this applies everywhere. Returns false, changing nothing, when `phys` is
/// not mapped or its page (up to 2 MiB) also covers usable RAM.
pub fn uncache_phys_map(phys: u64) -> bool {
    let va = super::physical_offset().as_u64().wrapping_add(phys);
    let Some((entry, size)) = kernel_leaf(va) else {
        return false;
    };
    let start = phys & !(size - 1);
    if size > 2 * 1024 * 1024 || overlaps_ram(start, start + size) {
        return false;
    }
    // SAFETY: `entry` is a present leaf of the kernel table (just walked);
    // setting the cache-disable bits changes only the memory type of the page,
    // never its address or permissions.
    unsafe { entry.write_volatile(entry.read_volatile() | pte::PCD | pte::PWT) };
    x86_64::instructions::tlb::flush(VirtAddr::new(va));
    // Lines cached under the old write-back type must not be written back
    // over device registers later.
    // SAFETY: `wbinvd` writes back and invalidates the caches; ring 0 only.
    unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };
    true
}

/// Base of the kernel MMIO window once chosen (0: none yet) and the bytes of
/// it handed out so far.
static KERNEL_WINDOW: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static KERNEL_WINDOW_USED: spin::Mutex<u64> = spin::Mutex::new(0);
/// Bytes the kernel MMIO window spans: one PDPT entry.
const KERNEL_WINDOW_SPAN: u64 = 1 << 30;

/// Map `len` bytes of device memory at `phys` (page aligned) for an
/// in-kernel driver, uncached, and return the kernel virtual address.
///
/// The bootloader's physical-memory map covers RAM and the first 4 GiB, but
/// firmware (OVMF in particular) places 64-bit BARs far above both, and its
/// large pages cannot be made uncached without retyping RAM beside the BAR.
/// So kernel drivers get their own 4 KiB uncached pages in a window: a free
/// 1 GiB slot of the page-table subtree that holds the kernel image. That
/// PML4 entry is in the kernel half every address space copies, so the
/// mapping is visible from any task's syscalls, whenever it is made.
/// Mappings are never removed (in-kernel drivers live as long as the
/// kernel). Refuses a range that overlaps RAM or lies beyond the CPU's
/// physical address width.
pub fn map_kernel(phys: u64, len: u64) -> Result<u64, &'static str> {
    if !phys.is_multiple_of(FRAME_SIZE) || len == 0 {
        return Err("unaligned device window");
    }
    let end = phys.checked_add(len).ok_or("device window wraps")?;
    if end > phys_limit() {
        return Err("device window beyond the physical address width");
    }
    if overlaps_ram(phys, end) {
        return Err("device window overlaps RAM");
    }
    let pages = len.div_ceil(FRAME_SIZE);
    let mut used = KERNEL_WINDOW_USED.lock();
    if *used + pages * FRAME_SIZE > KERNEL_WINDOW_SPAN {
        return Err("kernel MMIO window full");
    }
    let base = kernel_window()?;
    let va = base + *used;
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH
        | PageTableFlags::BIT_10;
    for page in 0..pages {
        let offset = page * FRAME_SIZE;
        if !map_page_in(
            super::kernel_table(),
            VirtAddr::new(va + offset),
            PhysAddr::new(phys + offset),
            flags,
        ) {
            // The pages mapped so far stay reserved (never reused), which
            // costs address space only.
            *used += offset;
            return Err("no frame for a page table");
        }
    }
    *used += pages * FRAME_SIZE;
    Ok(va)
}

/// The kernel MMIO window, choosing the highest free 1 GiB slot of the
/// kernel image's PML4 entry on first use (`fbwindow` picks its slot the
/// same way; each sees the other's slot as present once it maps a page).
fn kernel_window() -> Result<u64, &'static str> {
    use core::sync::atomic::Ordering;
    const PRESENT: u64 = 1;
    const ADDR: u64 = 0x000F_FFFF_FFFF_F000;
    let chosen = KERNEL_WINDOW.load(Ordering::Relaxed);
    if chosen != 0 {
        return Ok(chosen);
    }
    let anchor = kernel_window as *const () as u64;
    let index = (anchor >> 39) & 0x1FF;
    if index < 256 {
        return Err("kernel image outside the kernel half");
    }
    let pml4 = super::phys_to_virt(super::kernel_table()).as_ptr::<u64>();
    // SAFETY: the live kernel PML4, reached through the physical-memory map;
    // `index` is below 512.
    let entry = unsafe { pml4.add(index as usize).read_volatile() };
    if entry & PRESENT == 0 {
        return Err("kernel image not mapped");
    }
    let pdpt = super::phys_to_virt(PhysAddr::new(entry & ADDR)).as_ptr::<u64>();
    // The slot is claimed by mapping into it: a free slot stays free until
    // the caller maps its first page, which happens before anyone else can
    // look (one CPU, called with the window lock held).
    // SAFETY: a present PML4 entry of a 4-level table names a PDPT frame,
    // and `slot` is below 512.
    let free = (0..512u64)
        .rev()
        .find(|&slot| unsafe { pdpt.add(slot as usize).read_volatile() } & PRESENT == 0)
        .ok_or("no free slot beside the kernel image")?;
    // Sign-extend: the kernel half's addresses have bits 48..63 set.
    let base = 0xFFFF_0000_0000_0000 | index << 39 | free << 30;
    KERNEL_WINDOW.store(base, Ordering::Relaxed);
    Ok(base)
}
