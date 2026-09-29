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
