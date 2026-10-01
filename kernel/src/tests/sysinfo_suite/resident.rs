//! Version 5 of the syscall-14 snapshot: each task row's resident user
//! pages. The correctness test compares the row with a direct page-table
//! walk (also from inside the task's own address space), and the soak drives
//! map/snapshot/unmap rounds to show the count tracks every mapping and the
//! walk leaks nothing.

use super::*;

/// Where the resident-page tests map extra pages into a forked task.
const RSS_VA: u64 = 0x0080_0000;

/// The resident-page word of `slot`'s row.
fn resident_of(words: &[u64; sysinfo::WORDS], slot: usize) -> u64 {
    words[sysinfo::HEADER_WORDS + slot * sysinfo::TASK_ROW_WORDS + sysinfo::R_RESIDENT_PAGES]
}

/// Map `pages` fresh pages at [`RSS_VA`] into `table`, snapshot, unmap them,
/// and return the row's resident count while they were mapped.
fn resident_with(slot: usize, table: PhysAddr, pages: u64) -> Result<u64, String> {
    let end = RSS_VA + pages * 4096;
    process::map_range(table, RSS_VA, end).map_err(to_string)?;
    let words = snapshot();
    let cleared = mem::unmap_range(table, RSS_VA, end);
    mem::vma::remove(table, RSS_VA, end);
    check!(cleared as u64 == pages, "unmapped {cleared} of {pages} pages");
    Ok(resident_of(&*words?, slot))
}

/// Version 5 rows carry the task's resident user pages: a forked task's row
/// matches a direct walk of its table and follows pages mapped into it and
/// unmapped again; the kernel task, on the kernel table, reports none; an
/// empty slot reports none.
pub(super) fn snapshot_reports_resident_pages() -> Result<(), String> {
    fresh();
    let slot = task::spawn_fork().map_err(to_string)?;
    let pml4 = task::pml4_of(slot).ok_or("the fork has no address space")?;
    let table = PhysAddr::new(pml4);

    let words = snapshot()?;
    let base = resident_of(&words, slot);
    let walked = mem::user_table_frame_count(table) as u64;
    check!(
        base == walked,
        "the fork's row says {base} resident pages, a direct walk {walked}"
    );
    check!(
        resident_of(&words, task::KERNEL_TASK) == 0,
        "the kernel task reports {} resident pages",
        resident_of(&words, task::KERNEL_TASK)
    );
    let empty = (1..task::MAX_TASKS)
        .find(|&other| other != slot && task::pml4_of(other).is_none())
        .ok_or("no empty slot")?;
    check!(
        resident_of(&words, empty) == 0,
        "empty slot {empty} reports resident pages"
    );

    // The count must not depend on whose table is active: a monitor reads its
    // own row from inside its own address space.
    // (Kernel tasks report 0, so pages are mapped first to make 0 wrong.)
    process::map_range(table, RSS_VA, RSS_VA + 3 * 4096).map_err(to_string)?;
    let kernel = mem::kernel_table();
    mem::switch_to(table);
    let own = task::stats_snapshot().rows[slot].resident_pages as u64;
    mem::switch_to(kernel);
    mem::unmap_range(table, RSS_VA, RSS_VA + 3 * 4096);
    mem::vma::remove(table, RSS_VA, RSS_VA + 3 * 4096);
    check!(
        own == base + 3,
        "with its own table active the fork's row says {own}, expected {}",
        base + 3
    );

    let mapped = resident_with(slot, table, 5)?;
    check!(
        mapped == base + 5,
        "after mapping 5 pages the row says {mapped}, expected {}",
        base + 5
    );
    let after = resident_of(&*snapshot()?, slot);
    check!(
        after == base,
        "after unmapping the row says {after}, expected {base}"
    );
    task::harness::reset();
    Ok(())
}

/// Resident counts under sustained map/snapshot/unmap churn in one task: every
/// round reports exactly what is mapped, and the walk itself allocates
/// nothing that outlives a snapshot (no frame or slab growth).
pub(super) fn soak_snapshot_resident_pages() -> Result<(), String> {
    fresh();
    const ROUNDS: u64 = 64;
    let slot = task::spawn_fork().map_err(to_string)?;
    let table = PhysAddr::new(task::pml4_of(slot).ok_or("the fork has no address space")?);
    let base = resident_of(&*snapshot()?, slot);
    // Map and unmap once so the page tables under `RSS_VA` exist before the
    // baseline: they are reaped only at teardown, by design.
    resident_with(slot, table, 8)?;
    let frames_before = mem::frame_stats().live();
    let slab_before = mem::slab::stats().live_bytes;
    for round in 0..ROUNDS {
        let pages = round % 8 + 1;
        let resident = resident_with(slot, table, pages)?;
        check!(
            resident == base + pages,
            "round {round}: {resident} resident pages, expected {}",
            base + pages
        );
    }
    let frames_after = mem::frame_stats().live();
    check!(
        frames_after == frames_before,
        "frame leak across {ROUNDS} rounds: {frames_before} -> {frames_after}"
    );
    let slab_after = mem::slab::stats().live_bytes;
    check!(
        slab_after == slab_before,
        "slab leak across {ROUNDS} rounds: {slab_before} -> {slab_after}"
    );
    task::harness::reset();
    Ok(())
}
