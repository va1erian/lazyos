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
