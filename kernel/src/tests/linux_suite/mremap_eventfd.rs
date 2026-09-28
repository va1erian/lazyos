//! `mremap` grow/shrink/move (plus a soak) and `eventfd` semantics.

use super::*;

/// `mremap` grows and shrinks in place, then relocates with `MAYMOVE` and
/// `MREMAP_FIXED`, keeping the pages' contents throughout.
pub fn mremap_grow_shrink_move() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let (vsz, frames) = crate::mem::vma_stats(table);
    let base = process::linux::MMAP_BASE;
    check!(
        mmap_fixed(base, 2 * PAGE) == base,
        "mmap did not land at {base:#x}"
    );
    fill(base, 0x11, 2 * PAGE as usize);

    // Grow 2 -> 4 pages in place: the free range above is claimed.
    check!(
        mremap(base, 2 * PAGE, 4 * PAGE, 0, 0) == base,
        "in-place grow moved the mapping"
    );
    check!(matches(base, 0x11, 2 * PAGE as usize), "grow lost data");
    // Safety: the grown page is mapped into the kernel's user half.
    check!(
        unsafe { (base as *const u8).add(2 * PAGE as usize).read_volatile() } == 0,
        "grown page is not demand-zero"
    );

    // Shrink 4 -> 1 page: the tail is gone, the head intact.
    check!(
        mremap(base, 4 * PAGE, PAGE, 0, 0) == base,
        "in-place shrink moved the mapping"
    );
    check!(matches(base, 0x11, PAGE as usize), "shrink lost data");
    check!(
        crate::mem::vma::find(table, base + PAGE).is_none(),
        "shrunk tail still has a VMA"
    );

    // Relocate 1 -> 2 pages with MAYMOVE; a blocker mapping above forces the
    // move instead of an in-place grow.
    check!(
        mmap_fixed(base + PAGE, PAGE) == base + PAGE,
        "blocker mmap failed"
    );
    let moved = mremap(base, PAGE, 2 * PAGE, MREMAP_MAYMOVE, 0);
    check!(moved != 0 && (moved as i64) > 0, "move returned {moved:#x}");
    check!(moved != base, "move kept the old address");
    check!(matches(moved, 0x11, PAGE as usize), "move lost data");
    check!(
        crate::mem::vma::find(table, base).is_none(),
        "old VMA survived the move"
    );

    // MREMAP_FIXED places the range exactly.
    let dest = process::linux::MMAP_BASE + 0x40_0000;
    check!(
        mremap(
            moved,
            2 * PAGE,
            2 * PAGE,
            MREMAP_MAYMOVE | MREMAP_FIXED,
            dest
        ) == dest,
        "fixed move did not land at {dest:#x}"
    );
    check!(matches(dest, 0x11, PAGE as usize), "fixed move lost data");

    check!(munmap(dest, 2 * PAGE) == 0, "cleanup munmap failed");
    check!(munmap(base + PAGE, PAGE) == 0, "blocker munmap failed");
    let (vsz_after, frames_after) = crate::mem::vma_stats(table);
    check!(
        vsz_after == vsz,
        "mremap leaked VMA bytes: {vsz_after} != {vsz}"
    );
    check!(
        frames_after == frames,
        "mremap leaked frames: {frames_after} != {frames}"
    );
    Ok(())
}

/// Soak: repeated map/grow/relocate/shrink/unmap generations must not leak
/// VMAs, frames or quota.
pub fn mremap_soak_churn() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let (vsz, frames) = crate::mem::vma_stats(table);
    for round in 0..1000u32 {
        let base = process::linux::MMAP_BASE + 0x100_0000 + (round as u64 % 8) * 0x1_0000;
        check!(
            mmap_fixed(base, 2 * PAGE) == base,
            "round {round}: mmap failed"
        );
        fill(base, round as u8, 2 * PAGE as usize);
        let grown = mremap(base, 2 * PAGE, 3 * PAGE, MREMAP_MAYMOVE, 0);
        check!(
            grown != 0 && (grown as i64) > 0,
            "round {round}: grow returned {grown:#x}"
        );
        check!(
            matches(grown, round as u8, PAGE as usize),
            "round {round}: relocated page lost data"
        );
        // Touch the newly grown page so a frame is actually resident.
        // Safety: within the relocated mapping.
        unsafe {
            (grown as *mut u8)
                .add(2 * PAGE as usize)
                .write_volatile(0x5A)
        };
        let shrunk = mremap(grown, 3 * PAGE, PAGE, MREMAP_MAYMOVE, 0);
        check!(
            shrunk != 0 && (shrunk as i64) > 0,
            "round {round}: shrink returned {shrunk:#x}"
        );
        check!(
            matches(shrunk, round as u8, PAGE as usize),
            "round {round}: shrunk page lost data"
        );
        check!(munmap(shrunk, PAGE) == 0, "round {round}: munmap failed");
    }
    let (vsz_after, frames_after) = crate::mem::vma_stats(table);
    check!(
        vsz_after == vsz,
        "soak leaked VMA bytes: {vsz_after} != {vsz}"
    );
    check!(
        frames_after == frames,
        "soak leaked frames: {frames_after} != {frames}"
    );
    Ok(())
}

/// `eventfd` read/write semantics: drain-to-zero, non-blocking `EAGAIN`,
/// `EINVAL` on `u64::MAX`, and `EFD_SEMAPHORE` decrements.
pub fn eventfd_semantics() -> Result<(), String> {
    fresh()?;
    let efd = process::linux::dispatch_for_test(290, 5, O_NONBLOCK, 0);
    check!((efd as i64) > 0, "eventfd2 returned {efd:#x}");
    let mut value = [0u8; 8];
    check!(read_fd(efd, &mut value) == 8, "eventfd read length");
    check!(u64::from_le_bytes(value) == 5, "eventfd initial value");
    check!(read_fd(efd, &mut value) == EAGAIN, "empty eventfd read");
    let three = 3u64.to_le_bytes();
    check!(write_fd(efd, &three) == 8, "eventfd write");
    check!(
        read_fd(efd, &mut value) == 8 && u64::from_le_bytes(value) == 3,
        "add then drain"
    );
    let max = u64::MAX.to_le_bytes();
    check!(write_fd(efd, &max) == EINVAL, "eventfd u64::MAX write");

    let sem = process::linux::dispatch_for_test(290, 2, EFD_SEMAPHORE | O_NONBLOCK, 0);
    check!((sem as i64) > 0, "semaphore eventfd returned {sem:#x}");
    for expected in [1u64, 1] {
        check!(read_fd(sem, &mut value) == 8, "semaphore read length");
        check!(u64::from_le_bytes(value) == expected, "semaphore value");
    }
    check!(read_fd(sem, &mut value) == EAGAIN, "drained semaphore");

    check!(task::fd_close(efd as usize), "close eventfd failed");
    check!(task::fd_close(sem as usize), "close semaphore failed");
    check!(fds_clean(), "eventfd test left a descriptor");
    Ok(())
}
