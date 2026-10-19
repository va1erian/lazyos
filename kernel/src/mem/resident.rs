//! Counting the pages resident in a range of an address space.
//!
//! A reservation ([`vma::Vma::reserve`]) is charged to the user-memory quota
//! by its resident pages, so when one is unmapped the kernel needs to know how
//! many it had. The walk skips every absent page-table entry, so an 8 GiB
//! reservation with a few pages touched costs a few table visits, not two
//! million lookups.

use super::*;

/// The present user 4 KiB leaves of `table` in `[start, end)`: the pages
/// [`unmap_range`] would clear there (shared copy-on-write pages count once
/// per address space, like [`user_table_frame_count`]).
pub fn resident_pages(table: PhysAddr, start: u64, end: u64) -> u64 {
    let start = start & !(FRAME_SIZE - 1);
    let end = end.min(USER_TOP);
    if end <= start {
        return 0;
    }
    // Safety: `table` is a PML4 of a live address space, whose entries (and
    // the tables they name) stay in place while one task of it is running a
    // syscall or fault: the caller is that task.
    unsafe { count_below(table.as_u64(), 4, 0, start, end) }
}

/// Present leaves below the page table `phys` of `level` (4 = PML4 .. 1 = PT)
/// that covers the addresses from `base`, restricted to `[start, end)`.
///
/// # Safety
/// `phys` must be a page table of `level`.
unsafe fn count_below(phys: u64, level: u8, base: u64, start: u64, end: u64) -> u64 {
    crate::arch::irq_window::poll_point();
    // What one entry of this level maps: 512 GiB, 1 GiB, 2 MiB, 4 KiB.
    let shift = 12 + 9 * (u32::from(level) - 1);
    let entries = entry_table(PhysAddr::new(phys));
    let first = if start > base {
        ((start - base) >> shift) as usize
    } else {
        0
    };
    let mut count = 0;
    for index in first..512 {
        let low = base + ((index as u64) << shift);
        if low >= end {
            break;
        }
        let entry = core::ptr::read_volatile(entries.add(index));
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            if entry & PTE_USER != 0 && entry & pte::MMIO == 0 {
                count += 1;
            }
        } else if entry & PTE_HUGE == 0 {
            count += count_below(entry & PTE_ADDR, level - 1, low, start, end);
        }
    }
    count
}

/// How many page-table frames mapping a page at `va` in `table` would have
/// to allocate (0 to 3: the PDPT, PD and PT levels that are not there yet).
///
/// A reservation is charged by its pages, so a fault that creates tables is
/// charged for them too: touching one page in each 2 MiB of a huge reservation
/// would otherwise pin a table frame per page for free. The tables stay until
/// the address space dies, and so does their charge.
pub fn missing_tables(table: PhysAddr, va: u64) -> u64 {
    let mut phys = table.as_u64();
    // Levels 4 (PML4) down to 2 (PD): the entry at `level` names the table of
    // `level - 1`; an absent one means every table below it is absent too.
    for level in (2..=4u8).rev() {
        let shift = 12 + 9 * (u32::from(level) - 1);
        let index = ((va >> shift) & 0x1ff) as usize;
        // Safety: `phys` is the PML4 of a live address space or a table a
        // present entry of it named; both are reachable through the physical
        // map and 512 entries long, and `index` is below 512.
        let entry =
            unsafe { core::ptr::read_volatile(entry_table(PhysAddr::new(phys)).add(index)) };
        if entry & PTE_PRESENT == 0 {
            return u64::from(level) - 1;
        }
        if entry & PTE_HUGE != 0 {
            return 0;
        }
        phys = entry & PTE_ADDR;
    }
    0
}
