//! The `mmap` family: `mmap`, `munmap`, `mprotect`, `brk` and `mremap`.
//!
//! All five work the same way underneath: update the [`crate::mem::vma`]
//! table first (the source of truth for what a range means and who owns the
//! quota charge for it), then make the page tables agree, deferring frames to
//! first touch wherever demand-zero is safe.

use x86_64::PhysAddr;

use crate::mem::vma::{Kind, Prot};
use crate::quota::{self, Resource};
use crate::task;

use super::errno::{err, EFAULT, EINVAL, ENODEV, ENOMEM};
use super::{BRK_BASE, BRK_LIMIT, MMAP_BASE, MMAP_LIMIT, PAGE};

const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

/// `mremap(2)` flags.
const MREMAP_MAYMOVE: u64 = 1;
const MREMAP_FIXED: u64 = 2;

fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// `mmap(addr, len, prot, flags)`: anonymous private memory only.
///
/// The range is recorded as an `Anon` VMA and populated lazily (demand-zero):
/// no frames are spent until a page is first touched, which matches Linux for
/// a buffer that is allocated and never (fully) used. `MAP_FIXED` replaces
/// whatever was mapped there before, VMAs and page tables alike.
pub(super) fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64) -> u64 {
    if flags & MAP_ANONYMOUS == 0 {
        return err(ENODEV); // file-backed mmap not supported yet
    }
    if len == 0 {
        return err(EINVAL);
    }
    let len = align_up(len, PAGE);
    let table = crate::mem::kernel_table();
    let mut base = if flags & MAP_FIXED != 0 {
        addr & !0xFFF
    } else {
        task::mmap_next().max(MMAP_BASE)
    };
    if flags & MAP_FIXED == 0 {
        // The bump pointer may point into a range another call grew (or a
        // moved mapping left behind): find the first free hole, like Linux's
        // unmapped-area search. Without this, a fresh `mmap` could silently
        // replace part of a live mapping.
        loop {
            let Some(end) = base.checked_add(len) else {
                return err(ENOMEM);
            };
            if end > MMAP_LIMIT {
                return err(ENOMEM);
            }
            let occupied = crate::mem::vma::find_range(table, base, end);
            if occupied.is_empty() {
                break;
            }
            match occupied.iter().map(|vma| vma.end).max() {
                Some(next) => base = align_up(next, PAGE),
                None => break,
            }
        }
    }
    let Some(end) = base.checked_add(len) else {
        return err(ENOMEM);
    };
    if end > MMAP_LIMIT {
        return err(ENOMEM);
    }
    let prot = Prot((prot & 0x7) as u8);
    // Per-uid user-memory quota (issue #103), charged before any VMA or page
    // table changes so a refusal leaves the address space untouched. The
    // closest Linux errno for "over the user's memory quota" is ENOMEM.
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let mut replaced = 0u64;
    if flags & MAP_FIXED != 0 {
        replaced = crate::mem::vma::remove(table, base, end)
            .iter()
            .map(|vma| vma.len())
            .sum();
        crate::mem::unmap_range(table, base, end);
    }
    crate::mem::vma::insert(table, base, end, prot, Kind::Anon);
    if flags & MAP_FIXED == 0 {
        task::set_mmap_next(end);
    }
    // A fixed mapping that replaced live ranges gives their charge back.
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    base
}

/// `munmap(addr, len)`: drop the VMAs and release any resident pages. Linux
/// accepts unmapping an unmapped range, so this always returns 0 for valid
/// arguments. COW frames only lose this address space's reference.
pub(super) fn sys_munmap(addr: u64, len: u64) -> u64 {
    if len == 0 {
        return err(EINVAL);
    }
    let start = addr & !0xFFF;
    let Some(end) = addr.checked_add(len).map(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let removed = crate::mem::vma::remove(table, start, end);
    if !removed.is_empty() {
        let bytes: u64 = removed.iter().map(|vma| vma.len()).sum();
        crate::mem::unmap_range(table, start, end);
        // Unmapping gives the user's quota back (issue #103).
        quota::release_for_slot(task::current(), Resource::UserMemory, bytes);
    }
    0
}

/// `mprotect(addr, len, prot)`: update the PTE flags for resident pages and
/// the VMA for the whole range, so pages faulted in later honor the new access
/// too. Resident COW pages are privatized first (their protection is per
/// address space). A range with no VMA is a no-op: the old shim ignored it, and
/// we have no signal to deliver an ENOMEM against anyway.
pub(super) fn sys_mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    if len == 0 {
        return err(EINVAL);
    }
    let start = addr & !0xFFF;
    let Some(end) = addr.checked_add(len).map(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let prot = Prot((prot & 0x7) as u8);
    let table = crate::mem::kernel_table();
    if crate::mem::vma::find_range(table, start, end).is_empty() {
        return 0;
    }
    if !crate::mem::protect_range(table, start, end, prot) {
        return err(ENOMEM);
    }
    crate::mem::vma::protect(table, start, end, prot);
    0
}

/// `brk(addr)`: move the heap break. Growth records a `Heap` VMA and defers
/// the frames to first touch; shrinking unmaps what lies above the new break.
pub(super) fn sys_brk(addr: u64) -> u64 {
    let current = task::brk();
    if addr == 0 || addr < BRK_BASE {
        return current;
    }
    let new = align_up(addr, PAGE);
    if new > BRK_LIMIT {
        return current;
    }
    let table = crate::mem::kernel_table();
    if new > current {
        // Per-uid user-memory quota (issue #103): charge the growth before the
        // VMA exists. A refusal reports the unchanged break, which is how a
        // caller detects a failed brk.
        if quota::charge_for_slot(task::current(), Resource::UserMemory, new - current).is_err() {
            return current;
        }
        crate::mem::vma::insert(table, current, new, Prot::READ | Prot::WRITE, Kind::Heap);
    } else if new < current {
        crate::mem::vma::remove(table, new, current);
        crate::mem::unmap_range(table, new, current);
        quota::release_for_slot(task::current(), Resource::UserMemory, current - new);
    }
    task::set_brk(new);
    new
}

/// `mremap(old_address, old_size, new_size, flags, new_address)`: grow, shrink
/// or relocate an existing mapping.
///
/// The whole `[old_address, old_address + old_size)` range must be exactly one
/// VMA. Shrinking and growing a heap/anonymous mapping in place just adjust the
/// VMA (new pages stay demand-zero); anything else with `MREMAP_MAYMOVE`
/// relocates the resident PTEs to a fresh range (`MREMAP_FIXED` places it
/// exactly). Overlapping source and destination ranges are refused with
/// `-EINVAL`; Linux supports them, but nothing here needs that yet.
pub(super) fn sys_mremap(old_addr: u64, old_size: u64, new_size: u64, flags: u64, new_addr: u64) -> u64 {
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
        .map(|end| align_up(end, PAGE))
    else {
        return err(EINVAL);
    };
    let Some(new_len) = new_size
        .checked_add(PAGE - 1)
        .map(|size| size & !(PAGE - 1))
    else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let Some(vma) = crate::mem::vma::find(table, old_addr) else {
        return err(EFAULT);
    };
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
                crate::mem::vma::remove(table, new_end, old_end);
                crate::mem::unmap_range(table, new_end, old_end);
                quota::release_for_slot(task::current(), Resource::UserMemory, old_end - new_end);
            }
            return old_addr;
        }

        // Growing: the pages above the old end are demand-zero (anonymous
        // memory), so a free range above the VMA can be claimed by extending it.
        let delta = new_len - old_len;
        let free_above = crate::mem::vma::find_range(table, old_end, old_end + delta).is_empty();
        if free_above && matches!(vma.kind, Kind::Anon | Kind::Heap) {
            if quota::charge_for_slot(task::current(), Resource::UserMemory, delta).is_err() {
                return err(ENOMEM);
            }
            crate::mem::vma::insert(table, old_addr, old_end + delta, vma.prot, vma.kind);
            // Keep the bump past a mapping that grew in place, so the next
            // `mmap` does not land on top of it.
            if task::mmap_next() < old_end + delta {
                task::set_mmap_next(old_end + delta);
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
        match choose_mremap_dest(table, new_len) {
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
    let extra = new_len.saturating_sub(old_len);
    if extra > 0 && quota::charge_for_slot(task::current(), Resource::UserMemory, extra).is_err() {
        return err(ENOMEM);
    }
    // Replacing a live destination range returns its charge (MAP_FIXED rules).
    let replaced: u64 = crate::mem::vma::remove(table, dest, dest_end)
        .iter()
        .map(|vma| vma.len())
        .sum();
    if replaced > 0 {
        crate::mem::unmap_range(table, dest, dest_end);
    }
    // Only `min(old, new)` pages move; a shrinking move drops the old tail.
    let keep = old_len.min(new_len);
    let mut offset = 0;
    while offset < keep {
        if crate::mem::remap_page(table, old_addr + offset, dest + offset).is_err() {
            // Roll the pages already moved back, then undo the charge. The
            // destination was free (or replaced on request), so only the move
            // needs undoing.
            let mut back = 0;
            while back < offset {
                let _ = crate::mem::remap_page(table, dest + back, old_addr + back);
                back += PAGE;
            }
            if extra > 0 {
                quota::release_for_slot(task::current(), Resource::UserMemory, extra);
            }
            return err(ENOMEM);
        }
        offset += PAGE;
    }
    crate::mem::vma::remove(table, old_addr, old_end);
    crate::mem::unmap_range(table, old_addr, old_end);
    crate::mem::vma::insert(table, dest, dest_end, vma.prot, vma.kind);
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    if new_len < old_len {
        quota::release_for_slot(task::current(), Resource::UserMemory, old_len - new_len);
    }
    if !fixed_elsewhere {
        task::set_mmap_next(dest_end);
    }
    dest
}

/// First free address at or above the bump pointer that fits `len`.
fn choose_mremap_dest(table: PhysAddr, len: u64) -> Option<u64> {
    let mut candidate = task::mmap_next().max(MMAP_BASE);
    loop {
        let end = candidate.checked_add(len)?;
        if end > MMAP_LIMIT {
            return None;
        }
        let occupied = crate::mem::vma::find_range(table, candidate, end);
        if occupied.is_empty() {
            return Some(candidate);
        }
        candidate = align_up(occupied.iter().map(|vma| vma.end).max()?, PAGE);
    }
}
