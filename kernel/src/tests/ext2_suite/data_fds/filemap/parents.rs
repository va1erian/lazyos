//! File pages that are read before they are written, in a region nothing had touched (issue #853): the
//! read maps the page read-only (`MAP_SHARED` waits for its first store, `MAP_PRIVATE` is copy-on-write),
//! and the store that follows must find writable table entries above the leaf as well. See
//! `mem_suite::table_parents` for the walk and the other paths.

use super::*;

pub fn read_then_write_keeps_writable_parents() -> Result<(), String> {
    on_both(read_then_write_on)
}

fn read_then_write_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/parents");
    put(&path, &pattern(4 * PAGE as usize, 5))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(&path)?;
    // Each mapping in a PD of its own (2 MiB apart), the way a loader's or an allocator's are.
    let (shared, private) = (BASE, BASE + 0x40_0000);
    check!(
        mmap(shared, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == shared,
        "mmap(MAP_SHARED)"
    );
    check!(
        mmap(private, 2 * PAGE, RW, MAP_PRIVATE | MAP_FIXED, fd, 0) == private,
        "mmap(MAP_PRIVATE)"
    );
    for (name, base) in [("shared", shared), ("private", private)] {
        for page in 0..2 {
            let va = base + page * PAGE;
            // The read: a read-only leaf, the first in its region.
            space.touch(va, false)?;
            check!(
                space.entry(va).ok_or("unmapped")? & PTE_WRITABLE == 0,
                "{name} page {page}: the read mapped a writable page"
            );
            parents_writable(space.table, va)
                .map_err(|e| format!("{name} page {page} after the read: {e}"))?;
            // The store: the leaf becomes writable in place (or is copied); the walk must allow it.
            space.touch(va, true)?;
            check!(
                space.entry(va).ok_or("unmapped")? & PTE_WRITABLE != 0,
                "{name} page {page}: the store left the page read-only"
            );
            parents_writable(space.table, va)
                .map_err(|e| format!("{name} page {page} after the store: {e}"))?;
        }
    }
    // Defence in depth: a read-only parent over a read-only leaf (no kernel builds one any more, but the
    // store must not depend on that). A shared page's first store repairs it in `shared_write_fault`
    // (`mprotect` leaves a shared leaf read-only, so it takes the fault path each time) ...
    let old = BASE + 0x80_0000;
    check!(
        mmap(old, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == old,
        "mmap(MAP_SHARED) for the repair"
    );
    space.touch(old, false)?;
    for level in 0..3 {
        make_parent_read_only(space.table, old, level)?;
        check!(
            parents_writable(space.table, old).is_err(),
            "the setup left the shared page's parent writable"
        );
        check!(mprotect(old, PAGE, PROT_READ) == 0, "mprotect(R)");
        check!(mprotect(old, PAGE, RW) == 0, "mprotect(RW)");
        space.touch(old, true)?;
        parents_writable(space.table, old)
            .map_err(|e| format!("shared, level {level}, after the store: {e}"))?;
    }
    // ... and a private page's first store, a copy-on-write fault, in `cow_resolve` (a fresh mapping in
    // its own 2 MiB region for each level, read first so its leaf is read-only COW).
    for level in 0..3usize {
        let va = BASE + 0xc0_0000 + level as u64 * 0x20_0000;
        check!(
            mmap(va, PAGE, RW, MAP_PRIVATE | MAP_FIXED, fd, 0) == va,
            "mmap(MAP_PRIVATE) for the repair"
        );
        space.touch(va, false)?;
        make_parent_read_only(space.table, va, level)?;
        check!(
            parents_writable(space.table, va).is_err(),
            "the setup left the private page's parent writable"
        );
        space.touch(va, true)?;
        parents_writable(space.table, va)
            .map_err(|e| format!("private, level {level}, after the store: {e}"))?;
    }
    // `mprotect` repairs one as well: a private copy that was writable, taken back and opened again.
    let va = BASE + 0x100_0000;
    check!(
        mmap(va, PAGE, RW, MAP_PRIVATE | MAP_FIXED, fd, 0) == va,
        "mmap(MAP_PRIVATE) for mprotect"
    );
    space.touch(va, true)?;
    check!(mprotect(va, PAGE, PROT_READ) == 0, "mprotect(R)");
    make_parent_read_only(space.table, va, 2)?;
    check!(mprotect(va, PAGE, RW) == 0, "mprotect(RW)");
    parents_writable(space.table, va).map_err(|e| format!("private mprotect: {e}"))?;
    check!(munmap(shared, 0x200_0000) == 0, "munmap");
    close(fd);
    Ok(())
}
