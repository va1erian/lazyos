//! Copy-on-write address-space cloning (`fork`), split out of `mem/mod.rs` so
//! that file does not keep growing (issue #194). MMIO leaves (issue #240) are
//! never inherited: a forked child does not own the parent's device claim.

use x86_64::PhysAddr;

use super::pte;
use super::{
    alloc_zeroed_frame, entry_table, free_table, free_user_table, kernel_table, new_user_table,
    share_frame, switch_to, vma, COW_BIT, PTE_ADDR, PTE_PRESENT, PTE_WRITABLE, USER_PML4_ENTRIES,
};

/// Share the private user window (PML4 entries `0..USER_PML4_ENTRIES`) of
/// `parent` with a fresh address space using copy-on-write: both keep the
/// same frames with an extra reference, read-only; the first writer gets a
/// private copy (see [`cow_fault`]). Flushes the parent's TLB. The entries
/// above the window are shared with the kernel by `new_user_table`.
pub fn clone_user_table(parent: PhysAddr) -> Option<PhysAddr> {
    let child = new_user_table()?;
    let mut failed = false;
    // Safety: we own both tables and every frame we touch.
    unsafe {
        let src = entry_table(parent);
        let dst = entry_table(child);
        for index in 0..USER_PML4_ENTRIES {
            let entry = *src.add(index);
            if entry & PTE_PRESENT == 0 {
                continue;
            }
            match cow_clone_level(entry & PTE_ADDR, 3) {
                Some(sub) => *dst.add(index) = sub | (entry & !PTE_ADDR),
                None => {
                    failed = true;
                    break;
                }
            }
        }
    }
    if failed {
        // `cow_clone_level` already released the failing subtree; the
        // entries cloned before it are released with the child's table. The
        // child has no VMA list yet, so `free_user_table` only drops frames.
        free_user_table(child);
    } else {
        // Fork inherits the parent's layout: the child can demand-fault and
        // `mprotect` exactly the same ranges.
        vma::clone_space(parent, child);
    }
    // Our own leaves may now be read-only (or were restored by a failed
    // clone), so drop stale writable TLB entries either way.
    switch_to(kernel_table());
    if failed {
        None
    } else {
        Some(child)
    }
}

/// Share `level` (3=PDPT .. 1=PT) into new tables, marking leaves COW in both
/// the source and the copy. On failure the partial copy is released, so a
/// failed fork leaks nothing.
///
/// # Safety
/// `src_phys` must be a page table of `level`.
unsafe fn cow_clone_level(src_phys: u64, level: u8) -> Option<u64> {
    crate::arch::irq_window::poll_point();
    let new_phys = alloc_zeroed_frame()?;
    let src = entry_table(PhysAddr::new(src_phys));
    let dst = entry_table(new_phys);
    for i in 0..512 {
        let entry = *src.add(i);
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            // Device MMIO is not RAM and belongs to the parent's claim: the
            // child simply does not get the mapping.
            // A DMA buffer is shared with a device, not copy-on-write memory:
            // the child does not get it either.
            if entry & (pte::MMIO | pte::DMA) != 0 {
                continue;
            }
            // Share the frame read-only and mark it copy-on-write in both.
            if !share_frame(PhysAddr::new(entry & PTE_ADDR)) {
                free_table(new_phys.as_u64(), level);
                return None;
            }
            *dst.add(i) = (entry & PTE_ADDR) | ((entry & !PTE_ADDR) & !PTE_WRITABLE) | COW_BIT;
            *src.add(i) = (entry & !PTE_WRITABLE) | COW_BIT;
        } else {
            match cow_clone_level(entry & PTE_ADDR, level - 1) {
                Some(sub) => *dst.add(i) = sub | (entry & !PTE_ADDR),
                None => {
                    free_table(new_phys.as_u64(), level);
                    return None;
                }
            }
        }
    }
    Some(new_phys.as_u64())
}
