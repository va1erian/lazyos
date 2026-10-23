//! Per-address-space memory accounting (`mem::accounting`): resident, private
//! and proportional sizes, with shared pages divided between their mappers.

use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

use super::*;
use crate::mem::accounting::{self, Class};

const PAGE: u64 = 4096;

fn measure_one(table: PhysAddr) -> accounting::Use {
    accounting::measure(&[table])[0]
}

fn user_flags() -> PageTableFlags {
    PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::WRITABLE
}

/// Private pages count in full in every column, and a space's tables are
/// counted apart.
pub fn accounting_private_pages_count_in_full() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let empty = measure_one(table);
    check!(
        empty.rss == 0 && empty.pss == 0 && empty.tables == 1,
        "an empty space measured {empty:?}"
    );
    process::map_range(table, TEST_VA, TEST_VA + 5 * PAGE).map_err(to_string)?;
    let used = measure_one(table);
    check!(
        used.rss == 5 && used.private == 5 && used.pss == 5 * PAGE,
        "five private pages measured {used:?}"
    );
    check!(used.tables >= 4, "the tables are not counted: {used:?}");
    check!(
        used.class_pss.iter().sum::<u64>() == used.pss
            && used.class_rss.iter().sum::<u64>() == used.rss,
        "the classes do not add up: {used:?}"
    );
    mem::free_user_table(table);
    Ok(())
}

/// A page two spaces map is divided between them; the shares add up to one
/// page, and neither space owns it privately. After one of them takes a
/// private copy (COW) the other owns the frame again.
pub fn accounting_shared_pages_are_divided() -> Result<(), String> {
    let parent = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(parent, TEST_VA, TEST_VA + 4 * PAGE).map_err(to_string)?;
    let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
    let both = accounting::measure(&[parent, child]);
    for (name, used) in [("parent", both[0]), ("child", both[1])] {
        check!(
            used.rss == 4 && used.private == 0 && used.pss == 4 * PAGE / 2,
            "the {name} of a fork measured {used:?}"
        );
    }
    check!(
        both[0].pss + both[1].pss == 4 * PAGE,
        "the shares of four shared pages do not add up to four pages"
    );
    check!(mem::cow_fault(child, TEST_VA), "cow_fault failed");
    let after = accounting::measure(&[parent, child]);
    check!(
        after[1].private == 1 && after[0].private == 1,
        "after one copy: parent {:?}, child {:?}",
        after[0],
        after[1]
    );
    check!(
        after[0].pss + after[1].pss == 5 * PAGE,
        "five frames are live, the shares add up to {}",
        (after[0].pss + after[1].pss) / PAGE
    );
    mem::free_user_table(child);
    mem::free_user_table(parent);
    Ok(())
}

/// A frame the page cache (or a memfd) also holds is not shared by that
/// reference: one mapper owns it, whatever the reference count says.
pub fn accounting_a_holder_is_not_a_mapper() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let pages = process::map_range(table, TEST_VA, TEST_VA + PAGE).map_err(to_string)?;
    let frame = PhysAddr::new(pages[0].1);
    check!(mem::share_frame(frame), "share_frame refused");
    check!(
        mem::frame_refcount(frame) == 2,
        "the holder's reference is missing"
    );
    let used = measure_one(table);
    check!(
        used.private == 1 && used.pss == PAGE,
        "a held, singly mapped page measured {used:?}"
    );
    mem::free_frame(frame);
    mem::free_user_table(table);
    Ok(())
}

/// The pages are told apart by what the range they lie in is for.
pub fn accounting_classes_follow_the_vmas() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, TEST_VA, TEST_VA + 3 * PAGE).map_err(to_string)?;
    let (stack, image) = (TEST_VA + PAGE, TEST_VA + 2 * PAGE);
    mem::vma::insert(table, stack, image, Prot(3), Kind::Stack);
    mem::vma::insert(table, image, image + PAGE, Prot(5), Kind::FileImage);
    let used = measure_one(table);
    check!(
        used.class_rss[Class::Stack.index()] == 1
            && used.class_rss[Class::Image.index()] == 1
            && used.class_rss[Class::Private.index()] == 1,
        "classes measured {:?}",
        used.class_rss
    );
    mem::free_user_table(table);
    Ok(())
}

/// Device memory and driver buffers are not the program's: a leaf marked so
/// is not counted.
pub fn accounting_skips_device_leaves() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let frame = mem::alloc_zeroed_frame().ok_or("no frame")?;
    // A BAR is no allocator frame, so the mark is what keeps it out.
    let flags = user_flags() | PageTableFlags::BIT_10;
    check!(
        mem::map_page_in(table, VirtAddr::new(TEST_VA), frame, flags),
        "map_page_in failed"
    );
    check!(measure_one(table).rss == 0, "an MMIO leaf was counted");
    // Teardown leaves a device leaf alone, so the frame is still ours to free.
    mem::free_user_table(table);
    mem::free_frame(frame);
    Ok(())
}

/// A buffer in the shared window holds no reference of its own per mapping
/// (the registry holds one), yet is divided between the spaces that map it.
pub fn accounting_window_buffers_are_divided() -> Result<(), String> {
    let (a, b) = (
        mem::new_user_table().ok_or("new_user_table failed")?,
        mem::new_user_table().ok_or("new_user_table failed")?,
    );
    let frame = mem::alloc_zeroed_frame().ok_or("no frame")?;
    let at = mem::SHARED_WINDOW_BASE;
    check!(
        mem::map_page_in(a, VirtAddr::new(at), frame, user_flags())
            && mem::map_page_in(b, VirtAddr::new(at), frame, user_flags()),
        "map_page_in failed"
    );
    let both = accounting::measure(&[a, b]);
    for used in both.iter() {
        check!(
            used.class_rss[Class::Shared.index()] == 1 && used.pss == PAGE / 2 && used.private == 0,
            "a buffer mapped twice measured {used:?}"
        );
    }
    // Alone it is the one mapper's, though the registry still holds the frame.
    mem::unmap_range(b, at, at + PAGE);
    let alone = measure_one(a);
    check!(
        alone.private == 1 && alone.pss == PAGE,
        "a buffer mapped once measured {alone:?}"
    );
    // `unmap_range` released a reference per leaf it cleared; give the second back.
    mem::share_frame(frame);
    mem::unmap_range(a, at, at + PAGE);
    mem::free_user_table(a);
    mem::free_user_table(b);
    Ok(())
}

/// Soak: many spaces forked from one, copied and torn down again, over many
/// rounds. The proportional sizes always add up to the frames the spaces hold
/// (nothing is counted twice or lost) and no frame leaks.
pub fn accounting_soak_forks_add_up() -> Result<(), String> {
    const ROUNDS: u32 = 60;
    const SPACES: usize = 6;
    const PAGES: u64 = 24;
    let live = mem::frame_stats().live();
    for round in 0..ROUNDS {
        let root = mem::new_user_table().ok_or("new_user_table failed")?;
        process::map_range(root, TEST_VA, TEST_VA + PAGES * PAGE).map_err(to_string)?;
        let mut tables = vec![root];
        for _ in 1..SPACES {
            tables.push(mem::clone_user_table(root).ok_or("clone_user_table failed")?);
        }
        // Every space copies a different number of pages.
        for (index, table) in tables.iter().enumerate() {
            for page in 0..index as u64 {
                check!(
                    mem::cow_fault(*table, TEST_VA + page * PAGE),
                    "round {round}: cow_fault failed"
                );
            }
        }
        let uses = accounting::measure(&tables);
        let frames: u64 = PAGES + (SPACES as u64 - 1) * (SPACES as u64) / 2;
        // A frame that only one space is left with was a COW page whose
        // other holders copied it: it is private again.
        let pss: u64 = uses.iter().map(|used| used.pss).sum();
        // Dividing a page by a count that does not divide 4096 rounds down,
        // by under a byte for each leaf.
        let leaves = PAGES * SPACES as u64;
        check!(
            pss <= frames * PAGE && pss + leaves > frames * PAGE,
            "round {round}: shares add up to {pss} bytes, {frames} frames are live"
        );
        for (index, used) in uses.iter().enumerate() {
            check!(
                used.rss == PAGES && used.private >= index as u64,
                "round {round}: space {index} measured {used:?}"
            );
        }
        for table in tables {
            mem::free_user_table(table);
        }
    }
    let leaked = mem::frame_stats().live().saturating_sub(live);
    check!(leaked == 0, "{leaked} frames leaked over {ROUNDS} rounds");
    Ok(())
}
