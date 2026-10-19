//! `mremap`: grow, shrink or relocate a mapping.
//!
//! The quota follows the pages: a counted mapping is charged by length, so
//! growing charges the difference and shrinking gives it back; a reservation
//! ([`crate::mem::vma::Vma::reserve`]) is charged by its resident pages, which
//! move with the mapping, so only the pages a shrink drops are given back.

use crate::mem::vma::Kind;
use crate::quota::{self, Resource};
use crate::task;

use super::errno::{err, EFAULT, EINVAL, ENOMEM};
use super::mem::{align_up, first_free_range};
use super::mmapvfs::unmap_vmas;
use super::overcommit::hopeless;
use super::{MMAP_LIMIT, PAGE};

/// `mremap(2)` flags.
const MREMAP_MAYMOVE: u64 = 1;
const MREMAP_FIXED: u64 = 2;

/// `mremap(old_address, old_size, new_size, flags, new_address)`: grow, shrink
/// or relocate an existing mapping.
///
/// The whole `[old_address, old_address + old_size)` range must be exactly one
/// VMA. Shrinking and growing a heap/anonymous mapping in place just adjust the
/// VMA (new pages stay demand-zero); anything else with `MREMAP_MAYMOVE`
/// relocates the resident PTEs to a fresh range (`MREMAP_FIXED` places it
/// exactly). Overlapping source and destination ranges are refused with
/// `-EINVAL`; Linux supports them, but nothing here needs that yet.
pub(super) fn sys_mremap(
    old_addr: u64,
    old_size: u64,
    new_size: u64,
    flags: u64,
    new_addr: u64,
) -> u64 {
    if old_addr & (PAGE - 1) != 0 || (flags & MREMAP_FIXED != 0 && new_addr & (PAGE - 1) != 0) {
        return err(EINVAL);
    }
    if old_size == 0 || new_size == 0 || flags & !(MREMAP_MAYMOVE | MREMAP_FIXED) != 0 {
        return err(EINVAL);
    }
    if flags & MREMAP_FIXED != 0 && flags & MREMAP_MAYMOVE == 0 {
        return err(EINVAL);
    }
    let Some(old_end) = old_addr
        .checked_add(old_size)
        .and_then(|end| align_up(end, PAGE))
    else {
        return err(EINVAL);
    };
    let Some(new_len) = align_up(new_size, PAGE) else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let Some(vma) = crate::mem::vma::find(table, old_addr) else {
        return err(EFAULT);
    };
    // Growth the machine could never back is refused before anything moves
    // (see `overcommit`); memfd and shared pages are accounted elsewhere.
    if matches!(vma.kind, Kind::Anon | Kind::Heap)
        && !vma.reserve
        && new_len > old_end - old_addr
        && hopeless(vma.prot, new_len - (old_end - old_addr))
    {
        return err(ENOMEM);
    }
    // The range may cover only part of a coalesced VMA (adjacent anonymous
    // mappings merge): split at both boundaries so it is exactly one VMA.
    if old_addr < vma.start || old_end > vma.end {
        return err(EFAULT);
    }
    if vma.start < old_addr {
        crate::mem::vma::split(table, old_addr);
    }
    if old_end < vma.end {
        crate::mem::vma::split(table, old_end);
    }
    let Some(vma) = crate::mem::vma::find(table, old_addr) else {
        return err(EFAULT);
    };
    let old_len = old_end - old_addr;
    let fixed_elsewhere = flags & MREMAP_FIXED != 0 && new_addr != old_addr;

    if !fixed_elsewhere {
        // Shrinking (or the same size): keep the base address and drop the tail.
        if new_len <= old_len {
            let new_end = old_addr + new_len;
            if new_end < old_end {
                let gone = unmap_vmas(table, new_end, old_end, true);
                quota::release_for_slot(task::current(), Resource::UserMemory, gone.charged);
            }
            return old_addr;
        }

        // Growing: the pages above the old end are demand-zero (anonymous
        // memory), so a free range above the VMA can be claimed by extending it.
        let delta = new_len - old_len;
        // In place only inside the mmap area: past `MMAP_LIMIT` lie the device
        // windows and the stack, never anonymous memory to claim. Beyond it
        // the mapping can still move.
        let grow_end = old_end
            .checked_add(delta)
            .filter(|&grow_end| grow_end <= MMAP_LIMIT);
        let free_above =
            grow_end.is_some_and(|end| crate::mem::vma::find_range(table, old_end, end).is_empty());
        if let (Some(grow_end), true, true) = (
            grow_end,
            free_above,
            matches!(vma.kind, Kind::Anon | Kind::Heap),
        ) {
            // A reservation grows for free: its pages are charged as they come.
            if !vma.reserve
                && quota::charge_for_slot(task::current(), Resource::UserMemory, delta).is_err()
            {
                return err(ENOMEM);
            }
            crate::mem::vma::insert_like(table, old_addr, grow_end, &vma);
            // Keep the bump past a mapping that grew in place, so the next
            // `mmap` does not land on top of it.
            if task::mmap_next() < grow_end {
                task::set_mmap_next(grow_end);
            }
            return old_addr;
        }
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return err(ENOMEM);
    }
    if new_len > old_len && !matches!(vma.kind, Kind::Anon | Kind::Heap) {
        return err(EINVAL); // a stack/file mapping cannot grow by relocation
    }

    let dest = if fixed_elsewhere {
        new_addr
    } else {
        match first_free_range(table, new_len) {
            Some(dest) => dest,
            None => return err(ENOMEM),
        }
    };
    let Some(dest_end) = dest.checked_add(new_len) else {
        return err(ENOMEM);
    };
    if dest_end > MMAP_LIMIT || (dest < old_end && old_addr < dest_end) {
        return err(EINVAL);
    }
    let extra = if vma.reserve {
        0
    } else {
        new_len.saturating_sub(old_len)
    };
    if extra > 0 && quota::charge_for_slot(task::current(), Resource::UserMemory, extra).is_err() {
        return err(ENOMEM);
    }
    // Replacing a live destination range returns its charge (MAP_FIXED rules).
    let replaced = unmap_vmas(table, dest, dest_end, false).charged;
    // Only `min(old, new)` pages move; a shrinking move drops the old tail.
    let keep = old_len.min(new_len);
    let mut offset = 0;
    while offset < keep {
        crate::arch::irq_window::poll_point();
        // A reservation is mostly holes: jump over the absent tables.
        let hole = crate::mem::populated_from(table, old_addr + offset) - (old_addr + offset);
        if hole > 0 {
            offset += hole;
            continue;
        }
        if crate::mem::remap_page(table, old_addr + offset, dest + offset).is_err() {
            // Roll the pages already moved back, then undo the charge. What
            // the destination held is gone (it was replaced on request), so
            // its charge goes back too; the move is all that needs undoing.
            let mut back = 0;
            while back < offset {
                crate::arch::irq_window::poll_point();
                // Holes were skipped on the way out, so they are on the way
                // back (the destination's tables exist wherever a page moved).
                let hole = crate::mem::populated_from(table, dest + back) - (dest + back);
                if hole > 0 {
                    back += hole;
                    continue;
                }
                let _ = crate::mem::remap_page(table, dest + back, old_addr + back);
                back += PAGE;
            }
            if extra > 0 {
                quota::release_for_slot(task::current(), Resource::UserMemory, extra);
            }
            if replaced > 0 {
                quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
            }
            return err(ENOMEM);
        }
        offset += PAGE;
    }
    // A file mapping keeps its file: take it out before the old range goes.
    let file = crate::mem::vma::file_of(table, &vma);
    // What stays in the old range is what was not moved: a reservation held
    // exactly those pages (the moved ones carry their charge), a counted
    // mapping gives back the length that shrank away below.
    let dropped = unmap_vmas(table, old_addr, old_end, true);
    match file {
        Some(file) => crate::mem::vma::insert_file(
            table,
            dest,
            dest_end,
            vma.prot,
            vma.kind,
            file,
            vma.file_off,
        ),
        None => crate::mem::vma::insert_like(table, dest, dest_end, &vma),
    }
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    if vma.reserve {
        quota::release_for_slot(task::current(), Resource::UserMemory, dropped.charged);
    } else if new_len < old_len && vma.kind != Kind::Shared {
        quota::release_for_slot(task::current(), Resource::UserMemory, old_len - new_len);
    }
    if !fixed_elsewhere {
        task::set_mmap_next(dest_end);
    }
    dest
}
