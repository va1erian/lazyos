//! `mprotect`/`mremap`/`madvise` on shared mappings, the charge and frame
//! accounting, and a soak.

use super::*;

/// `mprotect` on a shared mapping changes the protection of the very pages
/// the other mapping sees, keeping them shared; `mremap` can shrink it and
/// `madvise(DONTNEED)` leaves its contents alone.
pub fn memfd_mprotect_mremap_madvise() -> Result<(), String> {
    fresh()?;
    let held = quota::usage(0, Resource::UserMemory);
    let a = Space::new()?;
    a.enter();
    let fd = memfd_create("prot", 0);
    check!(sys(77, &[fd, 4 * PAGE]) == 0, "ftruncate");
    let rw = PROT_READ | PROT_WRITE;
    check!(
        mmap(BASE, 4 * PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    a.store(BASE, b"keep")?;
    check!(mprotect(BASE, PAGE, PROT_READ) == 0, "mprotect read-only");
    let entry = a.entry(BASE)?;
    check!(
        entry & PTE_WRITABLE == 0 && entry & PTE_SHARED != 0 && entry & COW_BIT == 0,
        "read-only shared entry {entry:#x}"
    );
    check!(
        !mem::cow_fault(a.table, BASE),
        "a store to a read-only shared page was allowed"
    );
    check!(mprotect(BASE, PAGE, rw) == 0, "mprotect back");
    check!(a.entry(BASE)? & PTE_WRITABLE != 0, "not writable again");
    a.store(BASE, b"kept")?;
    check!(
        pread_fd(fd, 4, 0) == b"kept",
        "the file lost a store after mprotect"
    );

    // madvise(MADV_DONTNEED) keeps shared contents.
    check!(sys(28, &[BASE, 4 * PAGE, 4]) == 0, "madvise");
    check!(
        a.load(BASE, 4)? == b"kept",
        "DONTNEED dropped shared contents"
    );

    // mremap: shrinking works and gives no quota back; growing is refused.
    check!(
        sys(25, &[BASE, 4 * PAGE, 2 * PAGE, 0, 0]) == BASE,
        "mremap shrink"
    );
    check!(
        mem::vma::find(a.table, BASE + 2 * PAGE).is_none(),
        "the tail kept a VMA"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 4 * PAGE,
        "mremap released the memfd's charge: {}",
        quota::usage(0, Resource::UserMemory) - held
    );
    check!(
        sys(25, &[BASE, 2 * PAGE, 4 * PAGE, 0, 0]) == ENOMEM,
        "mremap grow"
    );
    check!(munmap(BASE, 2 * PAGE) == 0, "munmap");
    check!(sys(3, &[fd]) == 0, "close");
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "charge left behind"
    );
    Ok(())
}

/// Pages count once: the memfd holds the charge whoever maps it, the budget
/// bounds all memfds, and a process that exits mapped keeps nothing alive.
pub fn memfd_accounting() -> Result<(), String> {
    fresh()?;
    let held = quota::usage(0, Resource::UserMemory);
    let pages = live_pages();
    let frames = mem::frame_stats().live();
    let a = Space::new()?;
    a.enter();
    let fd = memfd_create("count", 0);
    check!(sys(77, &[fd, 16 * PAGE]) == 0, "ftruncate costs nothing");
    check!(
        quota::usage(0, Resource::UserMemory) == held && live_pages() == pages,
        "a hole was charged"
    );
    let rw = PROT_READ | PROT_WRITE;
    check!(
        mmap(BASE, 16 * PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 16 * PAGE && live_pages() == pages + 16,
        "mapping 16 pages: {} bytes, {} pages",
        quota::usage(0, Resource::UserMemory) - held,
        live_pages() - pages
    );
    let again = BASE + 0x10_0000;
    check!(
        mmap(again, 16 * PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == again,
        "second mapping"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 16 * PAGE,
        "a second mapping of the same pages was charged again"
    );
    // MAP_FIXED over a shared mapping releases the frames but not the memfd.
    check!(
        mmap(
            BASE,
            16 * PAGE,
            rw,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            !0,
            0
        ) == BASE,
        "replace a shared mapping with anonymous memory"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 32 * PAGE,
        "MAP_FIXED accounting: {}",
        quota::usage(0, Resource::UserMemory) - held
    );
    check!(munmap(BASE, 16 * PAGE) == 0, "munmap anonymous");
    // The address space dies with the mappings in place: frames return when
    // the descriptor also goes.
    drop(a);
    check!(sys(3, &[fd]) == 0, "close");
    check!(
        quota::usage(0, Resource::UserMemory) == held && live_pages() == pages,
        "charge or pages left: {} bytes, {} pages",
        quota::usage(0, Resource::UserMemory) - held,
        live_pages() - pages
    );
    check!(mem::frame_stats().live() == frames, "frames leaked");
    // The budget: one more page than all memfds may hold is `ENOSPC`.
    let big = memfd_create("big", 0);
    let budget = (crate::limits::scratch_max() / PAGE) as usize;
    check!(
        sys(77, &[big, (budget as u64 + 1) * PAGE]) == EFBIG,
        "a size past the budget"
    );
    check!(sys(3, &[big]) == 0, "close");
    Ok(())
}

/// Soak: thousands of create/grow/write/map/unmap/close cycles, with a
/// second address space mapping half of them, leak no frame, page, charge
/// or descriptor.
pub fn memfd_soak() -> Result<(), String> {
    fresh()?;
    let a = Space::new()?;
    let b = Space::new()?;
    let held = quota::usage(0, Resource::UserMemory);
    let rw = PROT_READ | PROT_WRITE;
    let cycle = |round: usize| -> Result<(), String> {
        let fd = memfd_create("soak", if round % 2 == 0 { MFD_CLOEXEC } else { 0 });
        check!((fd as i64) > 2, "round {round}: memfd_create {fd:#x}");
        let pages = (round % 5 + 1) as u64;
        check!(
            sys(77, &[fd, pages * PAGE]) == 0,
            "round {round}: ftruncate"
        );
        let byte = [round as u8];
        check!(
            sys(18, &[fd, byte.as_ptr() as u64, 1, 0]) == 1,
            "round {round}: pwrite"
        );
        a.enter();
        check!(
            mmap(BASE, pages * PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
            "round {round}: mmap in A"
        );
        a.store(BASE + PAGE * (pages - 1) + 1, &[round as u8 ^ 0x5a])?;
        if round % 2 == 0 {
            b.enter();
            check!(
                mmap(BASE, pages * PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
                "round {round}: mmap in B"
            );
            check!(
                b.load(BASE, 1)? == byte,
                "round {round}: B misses the file's byte"
            );
            check!(
                b.load(BASE + PAGE * (pages - 1) + 1, 1)? == [round as u8 ^ 0x5a],
                "round {round}: B misses A's store"
            );
            check!(
                munmap(BASE, pages * PAGE) == 0,
                "round {round}: munmap in B"
            );
        }
        a.enter();
        if round % 3 == 0 {
            // Close first: the mapping alone keeps the pages.
            check!(sys(3, &[fd]) == 0, "round {round}: close");
            check!(
                a.load(BASE, 1)? == byte,
                "round {round}: the mapping lost its data when the descriptor closed"
            );
            check!(
                munmap(BASE, pages * PAGE) == 0,
                "round {round}: munmap in A"
            );
        } else {
            check!(
                munmap(BASE, pages * PAGE) == 0,
                "round {round}: munmap in A"
            );
            check!(sys(3, &[fd]) == 0, "round {round}: close");
        }
        Ok(())
    };
    cycle(0)?;
    let pages = live_pages();
    let frames = mem::frame_stats().live();
    let fds = task::harness::fd_open_count(task::current());
    for round in 1..3000usize {
        cycle(round)?;
    }
    check!(
        live_pages() == pages && quota::usage(0, Resource::UserMemory) == held,
        "memfd pages {} -> {}, charge {} -> {}",
        pages,
        live_pages(),
        held,
        quota::usage(0, Resource::UserMemory)
    );
    check!(
        task::harness::fd_open_count(task::current()) == fds,
        "descriptors leaked"
    );
    let after = mem::frame_stats().live();
    check!(after == frames, "frames {frames} -> {after}");
    Ok(())
}
