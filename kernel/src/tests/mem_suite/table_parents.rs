//! The page-table entries above a leaf (issue #853). A CPU takes the lowest R/W bit of the whole walk,
//! so a user page the process may write needs writable PML4, PDPT and PD entries as well as a writable
//! leaf. `map_to` derives the parents from the *leaf's* flags: the first read-only leaf (a moved
//! copy-on-write page, a read-faulted `PROT_READ` page) in a region nothing had touched created
//! read-only parents, and the in-place paths that later make the leaf writable (`cow_resolve`,
//! `mprotect`, a shared file page's first store) never looked at them: the store faulted again with
//! the leaf writable, which no handler resolves (SIGSEGV; LazyWeb+ died at start-up about every
//! second launch).
//!
//! The tests work on user tables the way the fault handlers do (no store is made from ring 0 through
//! a bad walk: the CPU would fault in the kernel): they ask the walk itself.

use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use super::*;

const PAGE: u64 = 4096;
const TWO_MIB: u64 = 2 << 20;

/// A fork's child, its COW pages moved (`mremap`) to a `dest` in a region no mapping has touched.
fn forked_and_moved(pages: u64, dest: u64) -> Result<(PhysAddr, PhysAddr), String> {
    let parent = mem::new_user_table().ok_or("new_user_table failed")?;
    let mapped = process::map_range(parent, TEST_VA, TEST_VA + pages * PAGE).map_err(to_string)?;
    for (index, (_, phys)) in mapped.iter().enumerate() {
        fill_frame(*phys, page_seed(7, index));
    }
    let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
    for index in 0..pages {
        check!(
            mem::remap_page(child, TEST_VA + index * PAGE, dest + index * PAGE) == Ok(true),
            "remap_page of page {index} failed"
        );
    }
    Ok((parent, child))
}

/// `fork`, then `mremap(MAYMOVE)` of a written block to a destination that straddles a 2 MiB boundary
/// in untouched territory: every moved page, still read-only COW, sits under writable parents; the
/// store's copy makes the leaf writable and the walk stays writable; the contents survive.
pub fn remapped_cow_pages_keep_writable_parents() -> Result<(), String> {
    const PAGES: u64 = 3;
    // Two pages below the boundary and one above, in a PDPT slot nothing else uses.
    let dest = 0x4020_0000 - 2 * PAGE;
    let (parent, child) = forked_and_moved(PAGES, dest)?;
    for index in 0..PAGES {
        let va = dest + index * PAGE;
        let leaf = raw_entry(child, va).ok_or("a moved page is not mapped")?;
        check!(
            leaf & PTE_WRITABLE == 0 && leaf & COW_BIT != 0,
            "a moved COW page lost its COW mark: {leaf:#x}"
        );
        parents_writable(child, va)?;
        check!(
            mem::cow_fault(child, va),
            "the store's COW fault was refused"
        );
        let leaf = raw_entry(child, va).ok_or("the page vanished")?;
        check!(
            leaf & PTE_WRITABLE != 0,
            "the COW fault left the leaf read-only"
        );
        parents_writable(child, va)?;
        check!(
            frame_matches(frame_of(child, va)?, page_seed(7, index as usize)),
            "the moved page {index} lost its contents"
        );
    }
    mem::free_user_table(child);
    mem::free_user_table(parent);
    Ok(())
}

/// A page mapped read-only first in a fresh region (what a read of `PROT_READ` memory does), then
/// `mprotect`ed read-write: the walk is writable at every level afterwards.
pub fn mprotect_over_a_fresh_region_keeps_writable_parents() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let va = 0x6000_0000 + TWO_MIB; // its own PD, in a PDPT slot nothing else uses
    let frame = mem::alloc_zeroed_frame().ok_or("no frame")?;
    let read_only =
        PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
    check!(
        mem::map_page_in(table, VirtAddr::new(va), frame, read_only),
        "map_page_in failed"
    );
    parents_writable(table, va)?;
    check!(
        raw_entry(table, va).ok_or("unmapped")? & PTE_WRITABLE == 0,
        "the leaf must be read-only"
    );
    check!(
        mem::protect_range(table, va, va + PAGE, Prot::READ | Prot::WRITE),
        "protect_range refused"
    );
    check!(
        raw_entry(table, va).ok_or("unmapped")? & PTE_WRITABLE != 0,
        "mprotect(RW) left the leaf read-only"
    );
    parents_writable(table, va)?;
    mem::free_user_table(table);
    Ok(())
}

/// A table that an older kernel built (a read-only parent above a read-only leaf) is repaired where the
/// leaf becomes writable in place: `mprotect`, and the COW fault's two ways (a copy, and the sole
/// owner's shortcut).
pub fn writable_leaves_repair_read_only_parents() -> Result<(), String> {
    for level in 0..3 {
        // mprotect(RW) of a read-only page.
        let table = mem::new_user_table().ok_or("new_user_table failed")?;
        let va = 0x7000_0000;
        let frame = mem::alloc_zeroed_frame().ok_or("no frame")?;
        let read_only =
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        check!(
            mem::map_page_in(table, VirtAddr::new(va), frame, read_only),
            "map_page_in failed"
        );
        make_parent_read_only(table, va, level)?;
        check!(
            parents_writable(table, va).is_err(),
            "the setup left the parent writable"
        );
        check!(
            mem::protect_range(table, va, va + PAGE, Prot::READ | Prot::WRITE),
            "protect_range refused"
        );
        parents_writable(table, va).map_err(|e| format!("mprotect, level {level}: {e}"))?;
        mem::free_user_table(table);

        // The COW fault: a copy (the frame is shared with a second table) ...
        let (parent, child) = {
            let parent = mem::new_user_table().ok_or("new_user_table failed")?;
            process::map_range(parent, TEST_VA, TEST_VA + PAGE).map_err(to_string)?;
            let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
            (parent, child)
        };
        make_parent_read_only(child, TEST_VA, level)?;
        check!(
            mem::cow_fault(child, TEST_VA),
            "the copy's COW fault was refused"
        );
        parents_writable(child, TEST_VA).map_err(|e| format!("COW copy, level {level}: {e}"))?;
        // ... and the sole owner's shortcut (the child took its copy, so the parent is the only owner).
        make_parent_read_only(parent, TEST_VA, level)?;
        check!(
            mem::cow_fault(parent, TEST_VA),
            "the shortcut's COW fault was refused"
        );
        parents_writable(parent, TEST_VA)
            .map_err(|e| format!("COW shortcut, level {level}: {e}"))?;
        mem::free_user_table(child);
        mem::free_user_table(parent);
    }
    Ok(())
}

/// The growth pattern of a `Vec`: a written block of 0.2, 0.5, 1.5 and 1.8 MiB moved by `mremap` after a
/// fork, again and again, into destinations all over untouched territory (the boundary and the PD and
/// PDPT slots vary), in several processes at once. Every moved page, copied by the first store, is
/// writable through a writable walk, and nothing is leaked across the churn.
pub fn soak_remap_after_fork_keeps_writable_parents() -> Result<(), String> {
    const SIZES: [u64; 4] = [50, 128, 384, 460];
    const ROUNDS: u64 = 48;
    let free_before = mem::frame_stats().free;
    for round in 0..ROUNDS {
        let pages = SIZES[(round % 4) as usize];
        // A different 2 MiB slot, and a start shifted by a few pages so the block straddles one or
        // two boundaries; a different PDPT slot every 64 rounds.
        let dest = 0x4000_0000 + (round % 64) * TWO_MIB + (round / 64) * 0x4000_0000
            - (round % 5) * 7 * PAGE;
        let (parent, child) = forked_and_moved(pages, dest)?;
        // A second child of the same parent, its pages moved elsewhere: several processes at once.
        let sibling = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
        for index in 0..pages {
            check!(
                mem::remap_page(
                    sibling,
                    TEST_VA + index * PAGE,
                    dest + 0x800_0000 + index * PAGE
                ) == Ok(true),
                "the sibling's remap_page failed"
            );
        }
        for table in [child, sibling] {
            let base = if table == child {
                dest
            } else {
                dest + 0x800_0000
            };
            for index in 0..pages {
                let va = base + index * PAGE;
                parents_writable(table, va)
                    .map_err(|e| format!("round {round}, page {index}: {e}"))?;
                check!(
                    mem::cow_fault(table, va),
                    "round {round}: the COW fault of page {index} was refused"
                );
                parents_writable(table, va)
                    .map_err(|e| format!("round {round}, page {index} after the copy: {e}"))?;
            }
            check!(
                frame_matches(frame_of(table, base)?, page_seed(7, 0)),
                "round {round}: the first moved page lost its contents"
            );
        }
        mem::free_user_table(sibling);
        mem::free_user_table(child);
        mem::free_user_table(parent);
    }
    let free_after = mem::frame_stats().free;
    check!(
        free_after + 256 >= free_before,
        "the churn leaked frames: {free_before} free before, {free_after} after"
    );
    Ok(())
}
