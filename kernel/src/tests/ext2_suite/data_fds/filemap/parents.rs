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
    // A table as an older kernel built it, a read-only parent over the read-only shared leaf: the first
    // store repairs it.
    let (old, other) = (BASE + 0x80_0000, BASE + 0xc0_0000);
    check!(
        mmap(old, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == old,
        "mmap(MAP_SHARED) for the repair"
    );
    check!(
        mmap(other, PAGE, RW, MAP_PRIVATE | MAP_FIXED, fd, 0) == other,
        "mmap(MAP_PRIVATE) for the repair"
    );
    for (name, va) in [("shared", old), ("private", other)] {
        space.touch(va, false)?;
        for level in 0..3 {
            make_parent_read_only(space.table, va, level)?;
            check!(
                parents_writable(space.table, va).is_err(),
                "the setup left {name}'s parent writable"
            );
            // A fresh store each time: take the leaf back to its read-only state first.
            check!(mprotect(va, PAGE, PROT_READ) == 0, "mprotect(R)");
            check!(mprotect(va, PAGE, RW) == 0, "mprotect(RW)");
            space.touch(va, true)?;
            parents_writable(space.table, va)
                .map_err(|e| format!("{name}, level {level}, after the store: {e}"))?;
        }
    }
    check!(munmap(shared, 0x100_0000) == 0, "munmap");
    close(fd);
    Ok(())
}
