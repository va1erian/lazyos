//! Tearing a user mapping down: `unmap_range` (frames freed, the other CPUs
//! flushed before they are) and `remap_page` (`mremap`), split out of `uspace.rs`
//! to keep that file under its budget.

use super::*;

/// Drop `[start, end)` from `table`'s user mappings: clear each present leaf
/// and return its frame to the allocator (shared COW frames just lose one
/// reference). Page tables are left in place; [`free_user_table`] reaps them.
/// Returns the number of leaves cleared.
pub fn unmap_range(table: PhysAddr, start: u64, end: u64) -> usize {
    // Frames wait for the other CPUs' flush: one still writing through a
    // stale entry must not meet the frame's next owner.
    const CHUNK: usize = 32;
    let mut frames = [PhysAddr::new(0); CHUNK];
    let mut held = 0;
    let mut flush = tlb::Batch::new();
    let mut cleared = 0;
    let mut va = start & !(FRAME_SIZE - 1);
    let end = end.min(LOWER_HALF_END);
    while va < end {
        crate::arch::irq_window::poll_point();
        // Safety: `table` is a live address space and we own its entries;
        // `va < end <= LOWER_HALF_END`.
        let next = unsafe { skip_absent(table, va) };
        if next != va {
            va = next;
            continue;
        }
        // Safety: as above.
        if let Some(entry) = unsafe { leaf_entry(table, va) } {
            // Safety: `entry` was just returned as a present leaf in this table.
            let value = unsafe { *entry };
            // A device's BAR is not an allocator frame: its owner
            // (`mem::mmio`) unmaps it. A DMA leaf is one (`free_frame`
            // returns it to the pool), so it goes like any other.
            if value & PTE_USER != 0 && value & pte::MMIO == 0 {
                // Safety: same `entry`, still valid; nothing else can have
                // unmapped it in between (single-threaded teardown).
                unsafe { *entry = 0 };
                flush.note(va);
                frames[held] = PhysAddr::new(value & PTE_ADDR);
                held += 1;
                cleared += 1;
                if held == CHUNK {
                    flush.finish();
                    for &frame in frames.iter() {
                        free_frame(frame);
                    }
                    held = 0;
                }
            }
        }
        va += FRAME_SIZE;
    }
    flush.finish();
    for &frame in &frames[..held] {
        free_frame(frame);
    }
    cleared
}

/// Move the mapping of `old_va` to `new_va` without copying the frame: the
/// physical frame, COW bit and protection move with the PTE. Used by `mremap`
/// to relocate a mapping (`new_va` must be unmapped). `Ok(false)` means the
/// source page was not resident; `Err(())` means the destination page tables
/// could not be allocated (the source was restored).
pub fn remap_page(table: PhysAddr, old_va: u64, new_va: u64) -> Result<bool, ()> {
    let old_page = old_va & !(FRAME_SIZE - 1);
    let new_page = new_va & !(FRAME_SIZE - 1);
    // Safety: `table` is a live address space and we own its entries.
    let Some(old_entry) = (unsafe { leaf_entry(table, old_page) }) else {
        return Ok(false);
    };
    // Safety: `old_entry` was just returned as a present leaf in this table.
    let value = unsafe { *old_entry };
    if value & PTE_USER == 0 {
        return Ok(false);
    }
    // Safety: same entry, still valid; nothing else can have unmapped it in
    // between (single-threaded remap).
    unsafe { *old_entry = 0 };
    tlb::shootdown(old_page, 1);
    let flags = PageTableFlags::from_bits_truncate(value & !PTE_ADDR);
    if !map_page_in(
        table,
        VirtAddr::new(new_page),
        PhysAddr::new(value & PTE_ADDR),
        flags,
    ) {
        // Safety: the entry is still ours and was cleared just above.
        unsafe { *old_entry = value };
        tlb::shootdown(old_page, 1);
        return Err(());
    }
    Ok(true)
}
