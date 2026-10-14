//! The way WebKit's HTTP cache writes a body (issue #864, `WTF::FileSystem::createMappedFileData`,
//! `mapToFile`, `finalizeMappedFileData`): create the file with `O_EXCL`, `ftruncate` it, ask for
//! `fallocate`, map it `MAP_SHARED` read-write, `memcpy` the body in, `mprotect` it read-only, `msync`
//! it asynchronously, unmap and close; later the cache maps the file read-only (or `read`s it) and
//! hands the mapping to the web processes. The pieces were tested one by one; this is the sequence,
//! on a file whose size is not a page multiple, down to a remount.
//!
//! `fallocate` is `EOPNOTSUPP` on every file here: ext2 would have to reserve and zero real blocks
//! (a write as large as the body, the burst the call exists to avoid) to keep the promise "a store
//! through the mapping cannot fail for want of space". WTF accepts that answer and carries on with the
//! sparse file, which is what the mapping path below shows working.

use super::*;

const O_CREAT_EXCL_RW: u64 = O_CREAT | O_EXCL | O_RDWR;
const MS_ASYNC: u64 = 1;
const SYS_FALLOCATE: u64 = 285;
const SYS_PIPE: u64 = 22;
const EOPNOTSUPP: u64 = 95;
const FALLOC_FL_KEEP_SIZE: u64 = 1;
const FALLOC_FL_PUNCH_HOLE: u64 = 2;

/// A body of `len` bytes that differs per page and per byte.
fn body(len: usize) -> Vec<u8> {
    pattern(len, 77)
}

pub fn body_write_path() -> Result<(), String> {
    on_both(body_write_path_on)?;
    // On ext2 the body also survives the VFS's own flush and a remount.
    let data = Data::new(0)?;
    let expected = body(3 * PAGE as usize + 123);
    cache_sequence("/data/blob-remount", &expected)?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync");
    data.remount()?;
    check!(
        slurp("/data/blob-remount")? == expected,
        "the body did not survive a remount"
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
        let path = format!("{dir}/blob-{name}");
        cache_sequence(&path, &expected)?;
        check!(
            slurp(&path)? == expected,
            "{path}: read(2) sees another body"
        );
    }
    Ok(())
}

/// The cache's writer and then its reader, for the file at `path` and the bytes `data`.
fn cache_sequence(path: &str, data: &[u8]) -> Result<(), String> {
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
    // The reservation is refused the way WTF accepts.
    check!(
        syscall(SYS_FALLOCATE, fd, 0, 0, size) == errno(EOPNOTSUPP),
        "fallocate(mode 0) is not EOPNOTSUPP"
    );
    let rw = mmap(BASE, size, RW, MAP_SHARED | MAP_FIXED, fd, 0);
    check!(
        rw == BASE,
        "mmap(MAP_SHARED, RW) of {path} returned {rw:#x}"
    );
    // The memcpy, a byte at a time through the page-fault path.
    space.store(BASE, data)?;
    // finalizeMappedFileData: drop the write permission, flush asynchronously.
    check!(mprotect(BASE, size, PROT_READ) == 0, "mprotect(PROT_READ)");
    check!(
        !space.can_touch(BASE, true),
        "a store to the finalized body was allowed"
    );
    check!(
        space.load(BASE, data.len())? == data,
        "the mapping does not read back the body"
    );
    check!(msync(BASE, size, MS_ASYNC) == 0, "msync(MS_ASYNC)");
    check!(munmap(BASE, size) == 0, "munmap of the writer's mapping");
    check!(close(fd) == 0, "close");
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

pub fn fallocate_answers() -> Result<(), String> {
    on_both(fallocate_answers_on)
}

fn fallocate_answers_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/falloc");
    put(&path, &body(PAGE as usize))?;
    let rw = open_rw(&path)?;
    let ro = open_ro(&path)?;
    let size_of = |fd| fstat_size(fd);
    let before = size_of(rw)?;
    // A valid request is what the file system cannot do: every mode.
    for mode in [
        0,
        FALLOC_FL_KEEP_SIZE,
        FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
    ] {
        check!(
            syscall(SYS_FALLOCATE, rw, mode, 0, PAGE) == errno(EOPNOTSUPP),
            "fallocate(mode {mode}) is not EOPNOTSUPP"
        );
    }
    check!(
        size_of(rw)? == before,
        "a refused fallocate changed the size"
    );
    // A request that is wrong is wrong first: a closed descriptor, no length, a negative offset or
    // length, and a descriptor not open for writing.
    check!(
        syscall(SYS_FALLOCATE, 63, 0, 0, PAGE) == errno(EBADF),
        "bad descriptor"
    );
    check!(
        syscall(SYS_FALLOCATE, rw, 0, 0, 0) == errno(EINVAL),
        "len 0"
    );
    check!(
        syscall(SYS_FALLOCATE, rw, 0, u64::MAX, PAGE) == errno(EINVAL),
        "negative offset"
    );
    check!(
        syscall(SYS_FALLOCATE, rw, 0, 0, u64::MAX) == errno(EINVAL),
        "negative length"
    );
    check!(
        syscall(SYS_FALLOCATE, ro, 0, 0, PAGE) == errno(EBADF),
        "read-only descriptor"
    );
    // A pipe is not a file.
    let mut ends = [0u32; 2];
    check!(
        syscall(SYS_PIPE, ends.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe"
    );
    check!(
        syscall(SYS_FALLOCATE, ends[1] as u64, 0, 0, PAGE) == errno(ESPIPE),
        "fallocate on a pipe"
    );
    close(ends[0] as u64);
    close(ends[1] as u64);
    close(rw);
    close(ro);
    Ok(())
}

/// The cache's writer and reader, again and again (bodies of every size from one byte to a few
/// pages, each created, finalized, read back and unlinked): nothing is left mapped, open or allocated.
pub fn body_soak() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = crate::fs::openfile::open_files();
    // Warm up once: the first mapping carves page-cache and heap slabs.
    cache_sequence("/data/soak-warm", &body(PAGE as usize + 1))?;
    check!(path_call(SYS_UNLINK, "/data/soak-warm", 0) == 0, "unlink");
    let free_before = free_space()?;
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
        cache_sequence(&path, &expected)?;
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
    data.check_clean()
}
