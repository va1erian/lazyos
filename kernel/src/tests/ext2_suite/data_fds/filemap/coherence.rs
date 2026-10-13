//! Dirty tracking and write-back of shared file mappings: a read does not
//! dirty a page, the first store does, the flusher's non-waiting write-back
//! writes it and write-protects the mapping again; a fault that parks lets a
//! sibling unmap what the kernel validated; a change behind the cache is
//! picked up.

use super::*;
use crate::fs::vfs::{Filesystem, Id};

/// The open file behind descriptor `fd`.
fn file_of(fd: u64) -> Result<alloc::sync::Arc<crate::fs::openfile::OpenFile>, String> {
    match task::fd_clone(fd as usize) {
        Some(task::Fd::Vfs { ref file }) => Ok(alloc::sync::Arc::clone(file)),
        _ => Err(format!("descriptor {fd} is not a VFS file")),
    }
}

/// A read maps the page read-only and clean; the first store dirties it; a
/// non-waiting write-back (the flusher's) writes it, cleans it and
/// write-protects the mapping, so the next store dirties it again.
pub fn dirty_tracking_and_flusher() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/dirty";
    put(path, &pattern(2 * PAGE as usize, 3))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(path)?;
    let file = file_of(fd)?;
    check!(
        mmap(BASE, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    // Reads: read-only, never dirty, so the flusher has nothing to write.
    space.load(BASE, 8)?;
    space.load(BASE + PAGE, 8)?;
    check!(
        space.entry(BASE).ok_or("unmapped")? & PTE_WRITABLE == 0,
        "a read fault mapped a shared page writable"
    );
    check!(!file.has_dirty_pages(), "a read dirtied a page");
    // First store: write-protection fault, dirty, writable.
    space.store(BASE + 20, b"flushed by the flusher")?;
    check!(file.has_dirty_pages(), "a store did not dirty the page");
    check!(
        space.entry(BASE).ok_or("unmapped")? & PTE_WRITABLE != 0,
        "the page is not writable after its store"
    );
    check!(
        space.entry(BASE + PAGE).ok_or("unmapped")? & PTE_WRITABLE == 0,
        "the untouched page became writable"
    );
    // The flusher's write-back: no waiting, so a node-backed file only.
    crate::fs::openfile::writeback_all(false);
    check!(!file.has_dirty_pages(), "the write-back left the page dirty");
    check!(
        space.entry(BASE).ok_or("unmapped")? & PTE_WRITABLE == 0,
        "the mapping was not write-protected after the write-back"
    );
    let on_file = crate::fs::abi_read(Id::ROOT, path).map_err(|_| "read the file")?;
    check!(
        on_file[20..42] == *b"flushed by the flusher",
        "the flusher did not write the page to the file"
    );
    // A second store dirties it again (the page was re-protected).
    space.store(BASE + 100, b"again")?;
    check!(file.has_dirty_pages(), "a store after a write-back stayed clean");
    // Idle rounds write nothing and keep the page clean.
    crate::fs::openfile::writeback_all(false);
    crate::fs::openfile::writeback_all(false);
    check!(!file.has_dirty_pages(), "second write-back");
    check!(
        crate::fs::abi_read(Id::ROOT, path).map_err(|_| "read")?[100..105] == *b"again",
        "the second change was lost"
    );
    // mprotect(PROT_WRITE) of a read-only shared mapping makes no page
    // writable by itself: the first store still faults and dirties.
    check!(mprotect(BASE, 2 * PAGE, PROT_READ) == 0, "mprotect read");
    check!(mprotect(BASE, 2 * PAGE, RW) == 0, "mprotect write");
    check!(
        space.entry(BASE).ok_or("unmapped")? & PTE_WRITABLE == 0,
        "mprotect made a clean shared file page writable"
    );
    check!(munmap(BASE, 2 * PAGE) == 0, "munmap");
    close(fd);
    drop(file);
    drop(space);
    data.check_clean()
}

/// A fault that reads the file parks the task; a thread of the same space
/// unmaps an earlier page of the user buffer meanwhile. The kernel's copy
/// must notice and fail with `EFAULT` instead of writing into a page that
/// is gone. (The hook runs where a real fault would have been parked.)
pub fn parked_fault_cannot_leave_a_stale_translation() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/parked";
    put(path, &pattern(2 * PAGE as usize, 9))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(path)?;
    check!(
        mmap(BASE, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    // Page 0 present and writable; page 1 not yet faulted.
    space.store(BASE, b"x")?;
    check!(space.entry(BASE + PAGE).is_none(), "page 1 was mapped");
    fn sibling_unmaps_page_zero() {
        *mem::filemap::AFTER_FILE_READ.lock() = None;
        process::linux::dispatch_args_for_test(11, BASE, PAGE, 0, 0);
    }
    *mem::filemap::AFTER_FILE_READ.lock() = Some(sibling_unmaps_page_zero);
    let other = open_ro(path)?;
    let mut sink = [0u8; 16];
    let _ = &mut sink;
    // pread into a buffer spanning the two pages.
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let result = syscall(17, other, BASE + PAGE - 8, 16, 0);
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    *mem::filemap::AFTER_FILE_READ.lock() = None;
    check!(
        result == errno(EFAULT),
        "a copy whose buffer vanished while a fault parked returned {result:#x}"
    );
    check!(
        mem::vma::find(space.table, BASE).is_none(),
        "the hook did not unmap page 0"
    );
    // Without a sibling the same call works.
    check!(
        munmap(BASE + PAGE, PAGE) == 0
            && mmap(BASE, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "remap"
    );
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let result = syscall(17, other, BASE + PAGE - 8, 16, 0);
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    check!(result == 16, "the undisturbed copy returned {result:#x}");
    check!(munmap(BASE, 2 * PAGE) == 0, "munmap");
    close(other);
    close(fd);
    drop(space);
    data.check_clean()
}

/// A fault that loses a race to another mapper of the page gives its frame
/// back and leaves exactly the cache's and the winner's references.
pub fn lost_fault_race_keeps_no_extra_reference() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/race";
    put(path, &pattern(PAGE as usize, 5))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(path)?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    fn winner_maps_the_page() {
        *mem::filemap::AFTER_FILE_READ.lock() = None;
        let table = mem::kernel_table();
        let _ = mem::demand_fault(table, BASE, PageFaultErrorCode::empty());
    }
    *mem::filemap::AFTER_FILE_READ.lock() = Some(winner_maps_the_page);
    check!(
        mem::demand_fault(space.table, BASE, PageFaultErrorCode::empty()),
        "the losing fault was refused"
    );
    let frame = PhysAddr::new(frame_of(space.table, BASE)?);
    check!(
        mem::frame_refcount(frame) == 2,
        "frame holds {} references: the cache and one mapping were expected",
        mem::frame_refcount(frame)
    );
    check!(munmap(BASE, PAGE) == 0, "munmap");
    close(fd);
    drop(space);
    data.check_clean()
}

/// A write that reached the file behind the cache (a native write by name)
/// shows in the clean pages of a mapping once the cache is told.
pub fn change_behind_the_cache_is_refreshed() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/behind", &pattern(2 * PAGE as usize, 7))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_ro("/data/behind")?;
    check!(
        mmap(BASE, 2 * PAGE, PROT_READ, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.load(BASE, 4)?;
    space.load(BASE + PAGE, 4)?;
    data.fs
        .write("behind", 10, b"native write")
        .map_err(|_| "native write")?;
    check!(
        space.load(BASE + 10, 12)? != b"native write",
        "the mapping saw the write before it was told (it is a cache)"
    );
    crate::fs::openfile::content_changed("/data/behind", true);
    check!(
        space.load(BASE + 10, 12)? == b"native write",
        "the mapping did not pick up the native write"
    );
    data.fs
        .truncate("behind", PAGE + 20)
        .map_err(|_| "native truncate")?;
    crate::fs::openfile::content_changed("/data/behind", true);
    check!(
        space.load(BASE + PAGE + 30, 8)? == [0u8; 8],
        "the tail past a native truncate survived"
    );
    check!(munmap(BASE, 2 * PAGE) == 0, "munmap");
    close(fd);
    drop(space);
    data.check_clean()
}

/// A store lands while a write-back is between write-protecting the mapping
/// and writing the page: the file gets it (the frame is read at write
/// time), the page is dirty again, and the next write-back is a clean one.
pub fn store_during_writeback_is_not_lost() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/during";
    put(path, &pattern(PAGE as usize, 4))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(path)?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.store(BASE + 8, b"before")?;
    fn store_second() {
        *crate::fs::openfile::BEFORE_FLUSH.lock() = None;
        let table = mem::kernel_table();
        if raw_entry(table, BASE).is_some_and(|e| e & PTE_WRITABLE == 0) {
            assert!(mem::cow_fault(table, BASE), "write fault refused");
        }
        let frame = frame_of(table, BASE).unwrap();
        let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_mut_ptr::<u8>();
        // SAFETY: the frame is mapped by the physical-memory window.
        unsafe { core::ptr::copy_nonoverlapping(b"second".as_ptr(), ptr.add(300), 6) };
    }
    *crate::fs::openfile::BEFORE_FLUSH.lock() = Some(store_second);
    crate::fs::openfile::writeback_all(true);
    let on_file = crate::fs::abi_read(Id::ROOT, path).map_err(|_| "read")?;
    check!(on_file[8..14] == *b"before", "the first store is not on the file");
    check!(
        on_file[300..306] == *b"second",
        "a store during the write-back did not reach the file"
    );
    let file = file_of(fd)?;
    check!(
        file.has_dirty_pages(),
        "a store during a write-back left the page clean"
    );
    crate::fs::openfile::writeback_all(true);
    check!(!file.has_dirty_pages(), "the second write-back left it dirty");
    drop(file);
    check!(munmap(BASE, PAGE) == 0, "munmap");
    close(fd);
    drop(space);
    data.check_clean()
}

/// After `fork` both address spaces hold the shared page writable; a
/// write-back write-protects both, and a store in the child dirties the
/// page again and is seen by the parent.
pub fn fork_and_write_protect() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/forkwp";
    put(path, &pattern(PAGE as usize, 2))?;
    let parent = Space::new()?;
    parent.enter();
    let fd = open_rw(path)?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    parent.store(BASE, b"p")?;
    let table = mem::clone_user_table(parent.table).ok_or("fork")?;
    let child = parent.child(table);
    check!(
        parent.entry(BASE).ok_or("p")? & PTE_WRITABLE != 0
            && child.entry(BASE).ok_or("c")? & PTE_WRITABLE != 0,
        "fork did not keep the dirty shared page writable in both"
    );
    crate::fs::openfile::writeback_all(true);
    check!(
        parent.entry(BASE).ok_or("p")? & PTE_WRITABLE == 0
            && child.entry(BASE).ok_or("c")? & PTE_WRITABLE == 0,
        "a write-back left one of the two mappings writable"
    );
    child.store(BASE + 40, b"child store")?;
    check!(
        parent.load(BASE + 40, 11)? == b"child store",
        "the parent misses the child's store"
    );
    let file = file_of(fd)?;
    check!(file.has_dirty_pages(), "the child's store left the page clean");
    drop(file);
    crate::fs::openfile::writeback_all(true);
    check!(
        crate::fs::abi_read(Id::ROOT, path).map_err(|_| "read")?[40..51] == *b"child store",
        "the child's store did not reach the file"
    );
    drop(child);
    parent.enter();
    check!(munmap(BASE, PAGE) == 0, "munmap");
    close(fd);
    drop(parent);
    data.check_clean()
}

/// A task killed with a dirty shared page is reaped without waiting for the
/// volume: a page the teardown cannot write without waiting is parked, the
/// flusher (or `sync`) writes it. An unlinked file has nothing to write to
/// and parks nothing.
pub fn reaped_with_a_dirty_page_defers_the_write() -> Result<(), String> {
    let data = Data::new(0)?;
    for unlink in [false, true] {
        // A path-only (ramfs) file cannot be written without waiting, so the
        // reaper has to park the page.
        let path = "/tmp/reaped";
        put(path, &pattern(PAGE as usize, 6))?;
        let space = Space::new()?;
        space.enter();
        let fd = open_rw(path)?;
        check!(
            mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
            "mmap"
        );
        space.store(BASE + 16, b"left dirty")?;
        close(fd);
        if unlink {
            check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink");
        }
        // No flush_table: the task was killed, the reaper frees the space.
        drop(space);
        let parked = crate::fs::openfile::deferred_pages();
        if unlink {
            check!(parked == 0, "an unlinked file parked {parked} pages");
            continue;
        }
        check!(parked == 1, "{parked} pages parked, expected 1");
        check!(
            crate::fs::abi_read(Id::ROOT, path).map_err(|_| "read")?[16..26] != *b"left dirty",
            "the teardown waited and wrote the page itself"
        );
        crate::fs::openfile::writeback_all(false);
        check!(
            crate::fs::openfile::deferred_pages() == 1,
            "the non-waiting flusher wrote a page that needs a wait"
        );
        crate::fs::openfile::writeback_all(true);
        check!(
            crate::fs::openfile::deferred_pages() == 0,
            "a waiting write-back left the page parked"
        );
        check!(
            crate::fs::abi_read(Id::ROOT, path).map_err(|_| "read")?[16..26] == *b"left dirty",
            "the flusher did not write the parked page"
        );
    }
    data.check_clean()
}

/// Truncating a mapped file down and up again tears the mappings of the
/// dropped pages down: a store afterwards goes to a fresh page of the file,
/// not to a frame the file no longer owns.
pub fn truncate_down_and_up_unmaps_dropped_pages() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/shrink";
    put(path, &pattern(2 * PAGE as usize, 1))?;
    let (a, b) = (Space::new()?, Space::new()?);
    let fd = open_rw(path)?;
    for (space, at) in [(&a, BASE), (&b, BASE + 0x10_0000)] {
        space.enter();
        check!(
            mmap(at, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == at,
            "mmap"
        );
        space.load(at, 4)?;
        space.load(at + PAGE, 4)?;
    }
    check!(ftruncate(fd, 100) == 0, "ftruncate down");
    for (space, at) in [(&a, BASE), (&b, BASE + 0x10_0000)] {
        check!(space.entry(at).is_some(), "the kept page was unmapped");
        check!(
            space.entry(at + PAGE).is_none(),
            "a page past the new end is still mapped"
        );
        check!(
            !space.can_touch(at + PAGE, false),
            "a page past the end was mapped again"
        );
    }
    check!(ftruncate(fd, 2 * PAGE) == 0, "ftruncate up");
    a.enter();
    a.store(BASE + PAGE + 5, b"new page")?;
    check!(
        b.load(BASE + 0x10_0000 + PAGE + 5, 8)? == b"new page",
        "two mappers see different pages after a truncate down and up"
    );
    check!(
        pread_ok(fd, 8, PAGE + 5)? == b"new page",
        "read(2) does not see the store"
    );
    check!(
        a.load(BASE + PAGE + 100, 8)? == [0u8; 8],
        "the regrown page is not zero"
    );
    check!(munmap(BASE, 2 * PAGE) == 0, "munmap a");
    b.enter();
    check!(munmap(BASE + 0x10_0000, 2 * PAGE) == 0, "munmap b");
    close(fd);
    drop((a, b));
    data.check_clean()
}

/// Repeated parked faults with a sibling unmapping the buffer: nothing
/// leaks (frames, quota, cached pages) and every call fails cleanly.
pub fn parked_fault_soak() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/parksoak";
    put(path, &pattern(2 * PAGE as usize, 8))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(path)?;
    let other = open_ro(path)?;
    let before = mem::frame_stats().free;
    let held = quota::usage(0, Resource::UserMemory);
    fn sibling_unmaps_page_zero() {
        process::linux::dispatch_args_for_test(11, BASE, PAGE, 0, 0);
    }
    for round in 0..80 {
        check!(
            mmap(BASE, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
            "mmap round {round}"
        );
        space.store(BASE, b"x")?;
        *mem::filemap::AFTER_FILE_READ.lock() = Some(sibling_unmaps_page_zero);
        let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
        let result = syscall(17, other, BASE + PAGE - 8, 16, 0);
        crate::user_ptr::set_trust_kernel_pointers(trusted);
        *mem::filemap::AFTER_FILE_READ.lock() = None;
        check!(result == errno(EFAULT), "round {round}: {result:#x}");
        check!(munmap(BASE, 2 * PAGE) == 0, "munmap round {round}");
    }
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "quota leaked across the parked faults"
    );
    check!(
        mem::frame_stats().free + 4 >= before,
        "frames leaked across the parked faults: {before} -> {}",
        mem::frame_stats().free
    );
    close(other);
    close(fd);
    drop(space);
    data.check_clean()
}

/// A change behind the cache does not overwrite a page that was stored to.
pub fn refresh_keeps_a_stored_page() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/kept", &pattern(PAGE as usize, 9))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw("/data/kept")?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.store(BASE + 4, b"user store")?;
    data.fs
        .write("kept", 100, b"native")
        .map_err(|_| "native write")?;
    crate::fs::openfile::content_changed("/data/kept", true);
    check!(
        space.load(BASE + 4, 10)? == b"user store",
        "a refresh overwrote a stored page"
    );
    check!(munmap(BASE, PAGE) == 0, "munmap");
    close(fd);
    drop(space);
    data.check_clean()
}
