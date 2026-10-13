//! File mappings across `fork`, truncation, unmapping, `mremap` and a soak.

use super::*;

pub fn fork_keeps_mappings() -> Result<(), String> {
    on_both(fork_on)
}

fn fork_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/forked");
    let data = pattern(2 * PAGE as usize, 5);
    put(&path, &data)?;
    let parent = Space::new()?;
    parent.enter();
    let fd = open_rw(&path)?;
    let (shared, private) = (BASE, BASE + 0x10_0000);
    check!(
        mmap(shared, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == shared,
        "shared"
    );
    check!(
        mmap(private, 2 * PAGE, RW, MAP_PRIVATE | MAP_FIXED, fd, 0) == private,
        "private"
    );
    parent.store(shared, b"before")?;
    parent.load(private, 8)?;
    parent.load(private + PAGE, 8)?;
    let table = mem::clone_user_table(parent.table).ok_or("fork")?;
    let child = parent.child(table);
    // Shared: one frame, both see stores. Page 1 was never faulted in the
    // parent, so the child faults it from the same file.
    check!(
        frame_of(parent.table, shared)? == frame_of(child.table, shared)?,
        "the shared page was copied by fork"
    );
    parent.store(shared + 20, b"parent")?;
    check!(
        child.load(shared + 20, 6)? == b"parent",
        "the child misses the parent's shared store"
    );
    child.store(shared + 30, b"child")?;
    check!(
        parent.load(shared + 30, 5)? == b"child",
        "the parent misses the child's shared store"
    );
    check!(
        child.load(shared + PAGE, 4)? == data[PAGE as usize..PAGE as usize + 4],
        "child fault of a page the parent never touched"
    );
    // Private: copy on write on each side.
    let page1 = PAGE as usize;
    child.store(private + PAGE + 1, b"C")?;
    check!(
        parent.load(private + PAGE + 1, 1)? == data[page1 + 1..page1 + 2],
        "the child's private store leaked"
    );
    parent.store(private + PAGE + 2, b"P")?;
    check!(
        child.load(private + PAGE + 2, 1)? == data[page1 + 2..page1 + 3],
        "the parent's private store leaked"
    );
    drop(child);
    parent.enter();
    check!(munmap(shared, 2 * PAGE) == 0 && munmap(private, 2 * PAGE) == 0, "munmap");
    close(fd);
    let file = bytes_of(&path)?;
    check!(
        file[20..26] == *b"parent" && file[30..35] == *b"child",
        "shared stores did not reach the file"
    );
    check!(
        file[page1 + 1] == data[page1 + 1] && file[page1 + 2] == data[page1 + 2],
        "private stores reached the file"
    );
    Ok(())
}

pub fn truncation_and_unlink() -> Result<(), String> {
    on_both(truncation_on)
}

fn truncation_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/trunc");
    put(&path, &pattern(3 * PAGE as usize, 4))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(&path)?;
    check!(
        mmap(BASE, 3 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.load(BASE + 2 * PAGE, 1)?;
    space.load(BASE + PAGE + 10, 1)?;
    check!(munmap(BASE + 2 * PAGE, PAGE) == 0, "munmap tail");
    // Shrink into page 1: the rest of that page must read as zero when the
    // file grows back, and a new fault beyond the end is refused.
    check!(ftruncate(fd, PAGE + 100) == 0, "ftruncate shrink");
    check!(
        space.load(BASE + PAGE + 100, 4)? == [0u8; 4],
        "the tail of the last page kept stale bytes"
    );
    check!(
        mmap(BASE + 2 * PAGE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 2 * PAGE) == BASE + 2 * PAGE,
        "mapping past the end is allowed"
    );
    check!(
        !space.can_touch(BASE + 2 * PAGE, false),
        "a page past the end was mapped"
    );
    check!(ftruncate(fd, 3 * PAGE) == 0, "ftruncate grow");
    check!(
        space.load(BASE + 2 * PAGE, 8)? == [0u8; 8],
        "grown file does not read zeros"
    );
    check!(
        space.load(BASE + PAGE + 100, 8)? == [0u8; 8],
        "grown tail of the old last page is not zero"
    );
    // Unlink and close: the mapping keeps the data.
    let name = cstr(&path);
    close(fd);
    check!(
        syscall(SYS_UNLINK, name.as_ptr() as u64, 0, 0, 0) == 0,
        "unlink"
    );
    space.store(BASE, b"alive")?;
    check!(space.load(BASE, 5)? == b"alive", "unlinked mapping broken");
    check!(munmap(BASE, 3 * PAGE) == 0, "munmap");
    Ok(())
}

pub fn partial_unmap_and_remap() -> Result<(), String> {
    on_both(partial_on)
}

fn partial_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/partial");
    let data = pattern(6 * PAGE as usize, 11);
    put(&path, &data)?;
    let space = Space::new()?;
    space.enter();
    let fd = open_ro(&path)?;
    // Map pages 1..5 of the file, then punch out the middle.
    check!(
        mmap(BASE, 4 * PAGE, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, PAGE) == BASE,
        "mmap at offset"
    );
    check!(munmap(BASE + PAGE, PAGE) == 0, "punch");
    let upper = mem::vma::find(space.table, BASE + 2 * PAGE).ok_or("no upper VMA")?;
    check!(
        upper.file_off == 3 * PAGE,
        "the upper piece lost its offset: {upper:?}"
    );
    check!(
        space.load(BASE + 2 * PAGE + 9, 16)?
            == data[3 * PAGE as usize + 9..3 * PAGE as usize + 25],
        "the upper piece maps the wrong file page"
    );
    check!(
        space.load(BASE + 9, 16)? == data[PAGE as usize + 9..PAGE as usize + 25],
        "the lower piece maps the wrong file page"
    );
    // mprotect splits and re-merges without losing the offsets.
    check!(mprotect(BASE + 3 * PAGE, PAGE, RW) == 0, "mprotect");
    check!(
        space.load(BASE + 3 * PAGE, 4)? == data[4 * PAGE as usize..4 * PAGE as usize + 4],
        "page after mprotect"
    );
    // mremap moves a file mapping and keeps what it maps.
    let target = BASE + 0x20_0000;
    let moved = sys6(
        SYS_MREMAP,
        [BASE + 2 * PAGE, PAGE, PAGE, MREMAP_MAYMOVE | MREMAP_FIXED, target, 0],
    );
    check!(moved == target, "mremap {moved:#x}");
    check!(
        space.load(moved, 4)? == data[3 * PAGE as usize..3 * PAGE as usize + 4],
        "moved mapping changed content"
    );
    let rest = mem::vma::find(space.table, BASE + 3 * PAGE).ok_or("the page after the moved one")?;
    check!(
        rest.file_off == 4 * PAGE,
        "the neighbour's offset changed: {rest:?}"
    );
    check!(
        mem::vma::find(space.table, BASE + 2 * PAGE).is_none(),
        "the old place is still mapped"
    );
    close(fd);
    Ok(())
}

pub fn leaks_nothing() -> Result<(), String> {
    on_both(soak_on)
}

/// Map, touch, store, fork, unmap and exit many times: the frame pool, the
/// quota, the page cache and the open-file registry return to where they
/// started, and the data written is the data read.
fn soak_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/soak");
    let pages = 8u64;
    put(&path, &pattern((pages * PAGE) as usize, 1))?;
    let before = mem::frame_stats();
    let held = quota::usage(0, Resource::UserMemory);
    let open = crate::fs::openfile::open_files();
    for round in 0..60u8 {
        let space = Space::new()?;
        space.enter();
        let fd = open_rw(&path)?;
        let shared = round % 2 == 0;
        let flags = if shared { MAP_SHARED } else { MAP_PRIVATE } | MAP_FIXED;
        check!(
            mmap(BASE, pages * PAGE, RW, flags, fd, 0) == BASE,
            "mmap round {round}"
        );
        for page in 0..pages {
            let at = BASE + page * PAGE + u64::from(round);
            space.store(at, &[round, page as u8])?;
            check!(
                space.load(at, 2)? == [round, page as u8],
                "round {round} page {page} read back"
            );
        }
        let table = mem::clone_user_table(space.table).ok_or("fork")?;
        let child = space.child(table);
        child.store(BASE + 3, &[0xCC])?;
        drop(child);
        space.enter();
        if round % 3 == 0 {
            check!(msync(BASE, pages * PAGE, MS_SYNC) == 0, "msync");
        }
        let exits = round % 4 == 1;
        if !exits {
            check!(munmap(BASE, pages * PAGE) == 0, "munmap");
        }
        close(fd);
        drop(space);
        if exits {
            // The harness charges the kernel task's own space, which
            // dropping `space` does not tear down: give the charge back.
            quota::release_for_slot(task::current(), Resource::UserMemory, pages * PAGE);
        }
    }
    let after = mem::frame_stats();
    check!(
        after.free + 8 >= before.free,
        "frames leaked: {} -> {} free",
        before.free,
        after.free
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "quota leaked: {held} -> {}",
        quota::usage(0, Resource::UserMemory)
    );
    check!(
        crate::fs::openfile::open_files() == open,
        "open files leaked"
    );
    // The shared rounds' stores are in the file.
    let file = slurp(&path)?;
    check!(file.len() == (pages * PAGE) as usize, "size changed");
    check!(file[58] == 58 && file[59] == 0, "round 58 store lost");
    Ok(())
}
