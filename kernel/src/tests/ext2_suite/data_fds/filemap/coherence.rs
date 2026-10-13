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
