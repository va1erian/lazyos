//! Shared and private mappings: two address spaces, `fork`, copy-on-write.

use super::*;

/// Two address spaces map one memfd `MAP_SHARED`: they share frames and see
/// each other's stores, and the frame lives until the last reference (the
/// descriptor and each mapping) is gone.
pub fn memfd_shared_between_spaces() -> Result<(), String> {
    fresh()?;
    let held = quota::usage(0, Resource::UserMemory);
    let pages = live_pages();
    let a = Space::new()?;
    let b = Space::new()?;
    let fd = memfd_create("pair", 0);
    check!(sys(77, &[fd, 2 * PAGE]) == 0, "ftruncate");
    check!(write_fd(fd, b"from the file") == 13, "write");

    let (va, vb) = (BASE, BASE + 0x10_0000);
    a.enter();
    check!(
        mmap(
            va,
            2 * PAGE,
            PROT_READ | PROT_WRITE,
            MAP_SHARED | MAP_FIXED,
            fd,
            0
        ) == va,
        "mmap in A"
    );
    b.enter();
    check!(
        mmap(
            vb,
            2 * PAGE,
            PROT_READ | PROT_WRITE,
            MAP_SHARED | MAP_FIXED,
            fd,
            0
        ) == vb,
        "mmap in B"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 2 * PAGE,
        "pages must be charged once, not per mapping: {} -> {}",
        held,
        quota::usage(0, Resource::UserMemory)
    );
    let frame = a.frame(va)?;
    check!(
        b.frame(vb)? == frame,
        "different frames behind one memfd page"
    );
    check!(refs(frame) == 3, "frame references: {}", refs(frame));
    check!(
        a.entry(va)? & PTE_SHARED != 0 && a.entry(va)? & COW_BIT == 0,
        "A's entry is not a shared leaf: {:#x}",
        a.entry(va)?
    );
    check!(
        mem::vma::find(a.table, va).map(|vma| vma.kind) == Some(Kind::Shared),
        "no Shared VMA"
    );
    check!(
        a.load(va, 13)? == b"from the file",
        "A reads the file's bytes"
    );

    a.store(va + 100, b"written by A")?;
    check!(
        b.load(vb + 100, 12)? == b"written by A",
        "B misses A's store"
    );
    b.store(vb + PAGE + 7, b"written by B")?;
    check!(
        a.load(va + PAGE + 7, 12)? == b"written by B",
        "A misses B's store"
    );
    check!(
        pread_fd(fd, 12, 100) == b"written by A",
        "the file misses a store through a mapping"
    );
    check!(write_fd(fd, b"") == 0, "empty write");
    check!(
        sys(18, &[fd, b"direct".as_ptr() as u64, 6, 200]) == 6,
        "pwrite"
    );
    check!(
        b.load(vb + 200, 6)? == b"direct",
        "a write(2) is not visible in a mapping"
    );

    // The mapper's unmap gives no quota back (the memfd holds it) and leaves
    // the other mapping and the descriptor intact.
    a.enter();
    check!(munmap(va, 2 * PAGE) == 0, "munmap in A");
    check!(
        quota::usage(0, Resource::UserMemory) == held + 2 * PAGE,
        "munmap released the memfd's charge"
    );
    check!(refs(frame) == 2, "after munmap: {}", refs(frame));
    check!(b.load(vb + 100, 12)? == b"written by A", "B lost the data");
    // Closing the descriptor leaves B's mapping the frame's last owner.
    check!(sys(3, &[fd]) == 0, "close");
    check!(refs(frame) == 1, "after close: {}", refs(frame));
    check!(
        quota::usage(0, Resource::UserMemory) == held && live_pages() == pages,
        "closing the memfd kept its charge or pages"
    );
    check!(
        b.load(vb + 100, 12)? == b"written by A",
        "B lost the data after close"
    );
    // Exit of B: the frame returns.
    drop(b);
    check!(
        refs(frame) == 0,
        "the frame survived the last mapping: {}",
        refs(frame)
    );
    drop(a);
    Ok(())
}

/// `fork` keeps a shared mapping shared (and a shared anonymous one), while
/// a private page still copies on write.
pub fn memfd_shared_survives_fork() -> Result<(), String> {
    fresh()?;
    let a = Space::new()?;
    let fd = memfd_create("fork", 0);
    check!(sys(77, &[fd, PAGE]) == 0, "ftruncate");
    let (shared, private, anon) = (BASE, BASE + 0x10_0000, BASE + 0x20_0000);
    a.enter();
    let rw = PROT_READ | PROT_WRITE;
    check!(
        mmap(shared, PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == shared,
        "shared memfd"
    );
    check!(
        mmap(
            private,
            PAGE,
            rw,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            !0,
            0
        ) == private,
        "private anonymous"
    );
    check!(
        mmap(
            anon,
            PAGE,
            rw,
            MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED,
            !0,
            0
        ) == anon,
        "shared anonymous"
    );
    check!(
        mem::vma::find(a.table, anon).map(|vma| vma.kind) == Some(Kind::SharedAnon),
        "no SharedAnon VMA"
    );
    // The private page is demand-zero: touch it, as the program would.
    check!(
        mem::demand_fault(a.table, private, PageFaultErrorCode::CAUSED_BY_WRITE),
        "demand fault"
    );
    a.store(shared, b"S0")?;
    a.store(private, b"P0")?;
    a.store(anon, b"A0")?;

    let child_table = mem::clone_user_table(a.table).ok_or("clone_user_table failed")?;
    let child = a.child(child_table);
    let frame = a.frame(shared)?;
    check!(refs(frame) == 3, "memfd + parent + child: {}", refs(frame));
    for (va, what) in [(shared, "memfd"), (anon, "shared anonymous")] {
        check!(
            a.frame(va)? == child.frame(va)?,
            "{what}: the child has other frames"
        );
        let (parent_entry, child_entry) = (a.entry(va)?, child.entry(va)?);
        check!(
            parent_entry & COW_BIT == 0
                && child_entry & COW_BIT == 0
                && parent_entry & PTE_WRITABLE != 0
                && child_entry & PTE_WRITABLE != 0
                && child_entry & PTE_SHARED != 0,
            "{what}: fork made the mapping COW: parent {parent_entry:#x} child {child_entry:#x}"
        );
    }
    check!(
        a.entry(private)? & COW_BIT != 0 && child.entry(private)? & COW_BIT != 0,
        "the private page should be COW after fork"
    );

    check!(
        child.load(shared, 2)? == b"S0",
        "child reads the memfd page"
    );
    child.store(shared, b"S1")?;
    child.store(anon, b"A1")?;
    child.store(private, b"P1")?;
    check!(
        a.load(shared, 2)? == b"S1",
        "the parent misses the child's memfd store"
    );
    check!(
        a.load(anon, 2)? == b"A1",
        "the parent misses the child's shared-anonymous store"
    );
    check!(
        a.load(private, 2)? == b"P0",
        "a private page leaked into the parent"
    );
    a.store(shared, b"S2")?;
    check!(
        child.load(shared, 2)? == b"S2",
        "the child misses the parent's store"
    );
    check!(pread_fd(fd, 2, 0) == b"S2", "the file misses it");

    drop(child);
    check!(refs(frame) == 2, "after the child exits: {}", refs(frame));
    check!(sys(3, &[fd]) == 0, "close");
    check!(refs(frame) == 1, "after close: {}", refs(frame));
    drop(a);
    check!(refs(frame) == 0, "the frame outlived every owner");
    Ok(())
}

/// A private mapping of a memfd reads the file and copies on write: stores
/// never reach the file, also after `mprotect(PROT_WRITE)`; a read-only one
/// refuses the store.
pub fn memfd_private_mapping_copies_on_write() -> Result<(), String> {
    fresh()?;
    let held = quota::usage(0, Resource::UserMemory);
    let a = Space::new()?;
    let fd = memfd_create("private", 0);
    check!(write_fd(fd, b"file contents") == 13, "write");
    let va = BASE;
    a.enter();
    check!(
        mmap(
            va,
            PAGE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_FIXED,
            fd,
            0
        ) == va,
        "private mmap"
    );
    check!(
        quota::usage(0, Resource::UserMemory) > held,
        "a private mapping must be charged like anonymous memory"
    );
    check!(a.load(va, 13)? == b"file contents", "private read");
    let original = a.frame(va)?;
    check!(
        a.entry(va)? & PTE_SHARED == 0 && a.entry(va)? & COW_BIT != 0,
        "COW entry"
    );
    a.store(va, b"PRIVATE")?;
    check!(a.frame(va)? != original, "the store did not copy the page");
    check!(
        pread_fd(fd, 13, 0) == b"file contents",
        "a private store reached the file"
    );
    // A file change before the private copy is visible, after it is not.
    check!(sys(18, &[fd, b"X".as_ptr() as u64, 1, 12]) == 1, "pwrite");
    check!(
        a.load(va + 12, 1)? == [b's'],
        "the private copy followed the file"
    );

    // Read-only private mapping: the store is refused; mprotect(WRITE) leaves
    // the still-shared page read-only and COW (issue #773), and the first
    // store takes the copy, so the file stays clean.
    let ro = BASE + 0x10_0000;
    check!(
        mmap(ro, PAGE, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) == ro,
        "read-only private mmap"
    );
    let frame = a.frame(ro)?;
    check!(
        !mem::cow_fault(a.table, ro),
        "a store to a PROT_READ mapping was allowed"
    );
    check!(mprotect(ro, PAGE, PROT_READ | PROT_WRITE) == 0, "mprotect");
    check!(
        a.frame(ro)? == frame && a.entry(ro)? & PTE_WRITABLE == 0,
        "mprotect left the file's frame writable"
    );
    a.store(ro, b"mprotected")?;
    check!(a.frame(ro)? != frame, "the store after mprotect did not copy");
    check!(
        pread_fd(fd, 10, 0) == b"file conte",
        "mprotect then store reached the file"
    );

    check!(munmap(va, PAGE) == 0 && munmap(ro, PAGE) == 0, "munmap");
    check!(
        // The memfd's own page is still charged.
        quota::usage(0, Resource::UserMemory) == held + PAGE,
        "private charges did not come back: {} vs {}",
        quota::usage(0, Resource::UserMemory),
        held + PAGE
    );
    check!(sys(3, &[fd]) == 0, "close");
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "the memfd's page was not released"
    );
    Ok(())
}
