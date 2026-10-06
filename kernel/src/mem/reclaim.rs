//! Reclaiming page-table frames that an unmapped range left empty.
//!
//! [`super::unmap_range`] clears leaves but leaves the tables in place, which is
//! right for address-space teardown ([`super::free_user_table`] reaps them) but
//! leaks a page-table frame per range for a long-lived address space whose
//! mappings come and go (shared buffers, issue #237).

use x86_64::{PhysAddr, VirtAddr};

use super::free_frame;
use super::pte::{self, ADDR, HUGE, PRESENT};

const PT_SPAN: u64 = 1 << 21;
const PD_SPAN: u64 = 1 << 30;

/// True when all 512 entries of the table at `phys` are zero.
///
/// # Safety
/// `phys` must be a page table reachable through the physical memory map.
unsafe fn is_empty(phys: PhysAddr) -> bool {
    (0..512).all(|i| pte::read(phys, i) == 0)
}

/// Free the (level-1) page table and (level-2) page directory frames under
/// `[start, end)` that no longer hold any entry, clearing the entries that
/// named them. The PDPT (and PML4 entry) stay for the address space's next
/// mapping; [`free_window_tables`] reaps the shared-buffer window's PDPT when
/// the address space dies. Callers must have unmapped every leaf in the range.
pub fn reclaim_empty_tables(table: PhysAddr, start: u64, end: u64) {
    let mut block = start & !(PD_SPAN - 1);
    while block < end {
        // Safety: `table` is a live PML4 and each level is checked present
        // and non-huge before it is followed.
        unsafe { reclaim_directory(table, block, start, end) };
        block += PD_SPAN;
    }
}

/// Handle the 1 GiB block at `block` (PD-sized) intersecting `[start, end)`.
///
/// # Safety
/// `table` must be a live PML4 with stable lower levels.
unsafe fn reclaim_directory(table: PhysAddr, block: u64, start: u64, end: u64) {
    let index = |va: u64, shift: u64| ((va >> shift) & 0x1ff) as usize;
    let e4 = pte::read(table, index(block, 39));
    if e4 & PRESENT == 0 {
        return;
    }
    let p3 = PhysAddr::new(e4 & ADDR);
    let e3 = pte::read(p3, index(block, 30));
    if e3 & PRESENT == 0 || e3 & HUGE != 0 {
        return;
    }
    let pd = PhysAddr::new(e3 & ADDR);
    let mut span = block.max(start & !(PT_SPAN - 1));
    while span < end.min(block + PD_SPAN) {
        let slot = index(span, 21);
        let e2 = pte::read(pd, slot);
        if e2 & PRESENT != 0 && e2 & HUGE == 0 {
            let pt = PhysAddr::new(e2 & ADDR);
            if is_empty(pt) {
                pte::table(pd).add(slot).write_volatile(0);
                // INVLPG also drops cached paging-structure entries for `span`.
                x86_64::instructions::tlb::flush(VirtAddr::new(span));
                free_frame(pt);
            }
        }
        span += PT_SPAN;
    }
    if is_empty(pd) {
        pte::table(p3).add(index(block, 30)).write_volatile(0);
        x86_64::instructions::tlb::flush(VirtAddr::new(block));
        free_frame(pd);
    }
}

/// Free the page tables of an address space's shared-buffer window (PML4
/// entry 255): the PDPT at `pdpt` and every directory and table below it.
/// Each address space builds its own window on demand (`new_user_table` never
/// copies the entry), so the subtree is private to the dying table. Leaves are
/// *not* released: the shared-buffer registry owns their frame references and
/// has already dropped every mapping of this table (`ipc::shared::teardown_task`
/// runs first). A leaf still present here is a registry bug, reported and left
/// alone rather than released twice. Returns that count.
///
/// # Safety
/// `pdpt` must be the window PDPT of a table no task uses any more.
pub(super) unsafe fn free_window_tables(pdpt: PhysAddr) -> usize {
    let mut stale = 0;
    for slot in 0..512 {
        let e3 = pte::read(pdpt, slot);
        if e3 & PRESENT == 0 || e3 & HUGE != 0 {
            continue;
        }
        let pd = PhysAddr::new(e3 & ADDR);
        for dir in 0..512 {
            let e2 = pte::read(pd, dir);
            if e2 & PRESENT == 0 || e2 & HUGE != 0 {
                continue;
            }
            let pt = PhysAddr::new(e2 & ADDR);
            stale += (0..512)
                .filter(|&i| pte::read(pt, i) & PRESENT != 0)
                .count();
            free_frame(pt);
        }
        free_frame(pd);
    }
    free_frame(pdpt);
    stale
}
