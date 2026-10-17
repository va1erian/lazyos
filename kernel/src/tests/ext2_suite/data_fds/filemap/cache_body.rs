//! The way WebKit's HTTP cache writes a body (issue #864, `WTF::FileSystem::createMappedFileData`,
//! `mapToFile`, `finalizeMappedFileData`): create the file with `O_EXCL`, `ftruncate` it, map it
//! `MAP_SHARED` read-write, `memcpy` the body in, `mprotect` it read-only, `msync` it asynchronously,
//! unmap and close; later the cache maps the file read-only (or `read`s it) and hands the mapping to
//! the web processes. The pieces were tested one by one; this is the sequence, on files whose size is
//! not a page multiple, down to a remount, and a soak of it. It works: the shared write path is not
//! what keeps the cache from storing bodies as blobs.
//!
//! What does is `fallocate` (`ENOSYS`, so `createMappedFileData` gives up and the body is written
//! inline) and, behind it, hard links: `BlobStorage::add` links each blob under two names and
//! `BlobStorage::synchronize` deletes every blob whose `st_nlink` is 1 at the next start. `link` is
//! `EPERM` and `st_nlink` is always 1 here, so answering `EOPNOTSUPP` to `fallocate` (which WTF
//! accepts) makes the cache lose every large body at the next launch instead of writing it inline: the
//! LazyWeb+ harness's cache-hit check fails. `fallocate` stays as it is until hard links exist (issue
//! #871: an `ext2fs` link operation, VFS `link`, open-file and page-cache identity by inode rather than
//! path, and `unlink_open` parking when `nlink > 1`; then `fallocate` is `EOPNOTSUPP`).
//!
//! Both orders of the ending are tested: the writer's mapping dropped before the descriptor is closed,
//! and, as WTF does it (the handle is closed or handed on while the `MappedFileData` lives), the
//! descriptor closed first so the mapping outlives it.

use super::*;

const O_CREAT_EXCL_RW: u64 = O_CREAT | O_EXCL | O_RDWR;
const MS_ASYNC: u64 = 1;

/// A body of `len` bytes that differs per page and per byte.
fn body(len: usize) -> Vec<u8> {
    pattern(len, 77)
}

pub fn body_write_path() -> Result<(), String> {
    on_both(body_write_path_on)?;
    // On ext2 the body also survives the VFS's own flush and a remount (descriptor closed first, as WTF's
    // mapping outlives its handle).
    let data = Data::new(0)?;
    let expected = body(3 * PAGE as usize + 123);
    cache_sequence("/data/blob-remount", &expected, true)?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync");
    data.remount()?;
    check!(
        slurp("/data/blob-remount")? == expected,
        "the body did not survive a remount"
    );
    check!(
        path_call(SYS_UNLINK, "/data/blob-remount", 0) == 0,
        "unlink after the remount"
    );
    data.check_clean()
}

fn body_write_path_on(dir: &str) -> Result<(), String> {
    for (name, len) in [
        ("a", PAGE as usize + 1),
        ("b", 3 * PAGE as usize + 123),
        ("c", 5),
    ] {
        let expected = body(len);
        for (order, close_first) in [("unmap-first", false), ("close-first", true)] {
            let path = format!("{dir}/blob-{name}-{order}");
            cache_sequence(&path, &expected, close_first)?;
            check!(
                slurp(&path)? == expected,
                "{path}: read(2) sees another body"
            );
            check!(path_call(SYS_UNLINK, &path, 0) == 0, "{path}: unlink");
        }
    }
    Ok(())
}

/// The cache's writer and then its reader, for the file at `path` and the bytes `data`. With `close_first`
/// the descriptor is closed while the writer's mapping is still there (WTF's order); otherwise the mapping
/// goes first.
fn cache_sequence(path: &str, data: &[u8], close_first: bool) -> Result<(), String> {
    let size = data.len() as u64;
    let space = Space::new()?;
    space.enter();
    let fd = open_mode(path, O_CREAT_EXCL_RW, 0o600);
    check!(fd < 64, "create({path}) returned {fd:#x}");
    check!(
        open_mode(path, O_CREAT_EXCL_RW, 0o600) == errno(EEXIST),
        "a second O_EXCL create of {path} succeeded"
    );
    check!(ftruncate(fd, size) == 0, "ftruncate");
    let rw = mmap(BASE, size, RW, MAP_SHARED | MAP_FIXED, fd, 0);
    check!(
        rw == BASE,
        "mmap(MAP_SHARED, RW) of {path} returned {rw:#x}"
    );
    // The memcpy, a byte at a time through the page-fault path.
    space.store(BASE, data)?;
    // finalizeMappedFileData: drop the write permission, flush asynchronously.
    check!(mprotect(BASE, size, PROT_READ) == 0, "mprotect(PROT_READ)");
    // Every page is read-only now, not only the first.
    for page in 0..size.div_ceil(PAGE) {
        check!(
            !space.can_touch(BASE + page * PAGE, true),
            "a store to page {page} of the finalized body was allowed"
        );
    }
    check!(
        space.load(BASE, data.len())? == data,
        "the mapping does not read back the body"
    );
    check!(msync(BASE, size, MS_ASYNC) == 0, "msync(MS_ASYNC)");
    if close_first {
        // The mapping outlives the descriptor, as in WTF: the body must still be there.
        check!(close(fd) == 0, "close");
        check!(
            space.load(BASE, data.len())? == data,
            "the mapping lost the body when the descriptor was closed"
        );
        check!(munmap(BASE, size) == 0, "munmap of the writer's mapping");
    } else {
        check!(munmap(BASE, size) == 0, "munmap of the writer's mapping");
        check!(close(fd) == 0, "close");
    }
    // The body is in the file for read(2) ...
    check!(slurp(path)? == data, "read(2) does not see the body");
    // ... and for the next mapping of it: the reader, which hands pages to other processes.
    let ro = open_ro(path)?;
    check!(
        mmap(BASE, size, PROT_READ, MAP_SHARED | MAP_FIXED, ro, 0) == BASE,
        "the reader's mmap"
    );
    check!(
        space.load(BASE, data.len())? == data,
        "the reader's mapping does not show the body"
    );
    // A page of a reader's mapping of a read-only descriptor cannot be made writable.
    check!(
        mprotect(BASE, size, RW) == errno(EACCES),
        "mprotect(RW) of a read-only shared mapping"
    );
    check!(munmap(BASE, size) == 0, "munmap of the reader's mapping");
    close(ro);
    Ok(())
}

/// The cache's writer and reader, again and again (bodies of every size from one byte to a few
/// pages, each created, finalized, read back and unlinked): nothing is left mapped, open or allocated.
pub fn body_soak() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = crate::fs::openfile::open_files();
    // Warm up once: the first mapping carves page-cache and heap slabs.
    cache_sequence("/data/soak-warm", &body(PAGE as usize + 1), true)?;
    check!(path_call(SYS_UNLINK, "/data/soak-warm", 0) == 0, "unlink");
    let free_before = free_space()?;
    let held = quota::usage(0, Resource::UserMemory);
    for round in 0..160usize {
        // Sizes cycle through 1 byte, a page less one, a page, a page and a byte, and a few pages.
        let len = [
            1,
            PAGE as usize - 1,
            PAGE as usize,
            PAGE as usize + 1,
            3 * PAGE as usize + 7,
        ][round % 5];
        let dir = if round % 4 == 3 { "/tmp" } else { "/data" };
        let path = format!("{dir}/soak-{round}");
        let expected = pattern(len, round as u8);
        cache_sequence(&path, &expected, round % 2 == 0)?;
        check!(slurp(&path)? == expected, "round {round}: the body differs");
        check!(
            path_call(SYS_UNLINK, &path, 0) == 0,
            "round {round}: unlink"
        );
        if round % 40 == 39 {
            crate::trace::test!("linux_filemap_cache_body_soak:PROGRESS:round {round}/160");
        }
    }
    check!(
        free_space()? == free_before,
        "the soak changed the volume's free space: {:?} then {:?}",
        free_before,
        free_space()?
    );
    check!(
        crate::fs::openfile::open_files() == baseline,
        "the soak left files open"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "the soak kept user memory charged: {held} then {}",
        quota::usage(0, Resource::UserMemory)
    );
    data.check_clean()
}
