//! `MAP_PRIVATE`, `MAP_SHARED`, write-back and refusals of file mappings.

use super::*;

pub fn private_mapping() -> Result<(), String> {
    on_both(private_mapping_on)
}

/// `MAP_PRIVATE`: lazy, read from the file, copy-on-write, the file never
/// changes, later writes to the file show in pages not yet copied, bytes past
/// the end of the last page read as zero and pages wholly past it are refused.
fn private_mapping_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/priv");
    let data = pattern(3 * PAGE as usize + 100, 3);
    put(&path, &data)?;
    let space = Space::new()?;
    space.enter();
    let held = quota::usage(0, Resource::UserMemory);
    let fd = open_ro(&path)?;
    check!(
        mmap(BASE, 5 * PAGE, RW, MAP_PRIVATE | MAP_FIXED, fd, 0) == BASE,
        "mmap failed"
    );
    check!(
        quota::usage(0, Resource::UserMemory) == held + 5 * PAGE,
        "private file mapping must be charged like anonymous memory"
    );
    let vma = mem::vma::find(space.table, BASE).ok_or("no VMA")?;
    check!(
        vma.kind == Kind::FilePrivate && vma.file != 0,
        "VMA is {vma:?}"
    );
    for page in 0..5 {
        check!(
            space.entry(BASE + page * PAGE).is_none(),
            "page {page} was mapped eagerly"
        );
    }
    check!(
        space.load(BASE + PAGE, 64)? == data[PAGE as usize..PAGE as usize + 64],
        "page 1 differs from the file"
    );
    let entry = space.entry(BASE + PAGE).ok_or("page 1 not mapped")?;
    check!(
        entry & PTE_WRITABLE == 0 && entry & COW_BIT != 0,
        "a read of a private page must be COW read-only: {entry:#x}"
    );
    // A store copies; the file keeps its bytes.
    let shared = frame_of(space.table, BASE + PAGE)?;
    space.store(BASE + PAGE + 5, b"PRIVATE")?;
    check!(
        frame_of(space.table, BASE + PAGE)? != shared,
        "the store did not take a private copy"
    );
    check!(
        space.load(BASE + PAGE + 5, 7)? == b"PRIVATE",
        "the private copy lost the store"
    );
    check!(bytes_of(&path)? == data, "a private store reached the file");
    // A write to a page that was never read builds the copy directly.
    space.store(BASE + 2 * PAGE + 1, b"direct")?;
    check!(
        space.entry(BASE + 2 * PAGE).ok_or("unmapped")? & COW_BIT == 0,
        "a first-touch store left the page COW"
    );
    // The tail of the last page is zero past the end of the file.
    check!(
        space.load(BASE + 3 * PAGE + 100, 8)? == [0u8; 8],
        "bytes past EOF are not zero"
    );
    check!(
        space.load(BASE + 3 * PAGE, 100)? == data[3 * PAGE as usize..],
        "last partial page differs"
    );
    // Wholly past the end: refused.
    check!(
        !space.can_touch(BASE + 4 * PAGE, false),
        "a page wholly past EOF was mapped"
    );
    // A write(2) shows in a page that was not copied; not in a copied one.
    let writer = open_rw(&path)?;
    check!(pwrite(writer, b"WRITTEN", 10) == 7, "pwrite");
    check!(
        space.load(BASE + 10, 7)? == b"WRITTEN",
        "write(2) is not visible in a private mapping"
    );
    check!(pwrite(writer, b"LATER!!", PAGE + 5) == 7, "pwrite 2");
    check!(
        space.load(BASE + PAGE + 5, 7)? == b"PRIVATE",
        "a copied page changed under its owner"
    );
    close(writer);
    // munmap gives back the charge and the cache pages.
    check!(munmap(BASE, 5 * PAGE) == 0, "munmap");
    check!(
        quota::usage(0, Resource::UserMemory) == held,
        "munmap kept the charge"
    );
    close(fd);
    Ok(())
}

pub fn shared_mapping() -> Result<(), String> {
    on_both(shared_mapping_on)
}

/// `MAP_SHARED`: two address spaces share frames, stores are seen by `read`
/// at once and by the file after `msync`, `write(2)` shows in the mapping,
/// and what was stored survives the unmap.
fn shared_mapping_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/shared");
    let data = pattern(2 * PAGE as usize, 9);
    put(&path, &data)?;
    let (a, b) = (Space::new()?, Space::new()?);
    let (fda, fdb) = (open_rw(&path)?, open_rw(&path)?);
    a.enter();
    check!(
        mmap(BASE, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fda, 0) == BASE,
        "mmap in A"
    );
    b.enter();
    check!(
        mmap(
            BASE + 0x10_0000,
            2 * PAGE,
            RW,
            MAP_SHARED | MAP_FIXED,
            fdb,
            0
        ) == BASE + 0x10_0000,
        "mmap in B"
    );
    let vb = BASE + 0x10_0000;
    check!(
        b.load(vb + 3, 4)? == data[3..7],
        "B reads something else than the file"
    );
    check!(
        a.load(BASE + 3, 4)? == data[3..7],
        "A reads something else than the file"
    );
    check!(
        frame_of(a.table, BASE)? == frame_of(b.table, vb)?,
        "two mappings of one file page use different frames"
    );
    let entry = a.entry(BASE).ok_or("unmapped")?;
    // A read maps a shared page read-only; the first store makes it writable.
    check!(
        entry & (1 << 52) != 0 && entry & PTE_WRITABLE == 0 && entry & COW_BIT == 0,
        "not a read-only shared leaf after a read: {entry:#x}"
    );
    check!(
        mem::vma::find(a.table, BASE).map(|vma| vma.kind) == Some(Kind::FileShared),
        "no FileShared VMA"
    );
    // A's store is B's, and a read(2) sees it before any write-back.
    a.store(BASE + 100, b"stored by A")?;
    check!(
        a.entry(BASE).ok_or("unmapped")? & PTE_WRITABLE != 0,
        "a store left the shared page read-only"
    );
    check!(
        b.load(vb + 100, 11)? == b"stored by A",
        "B misses A's store"
    );
    check!(
        pread_ok(fdb, 11, 100)? == b"stored by A",
        "read(2) misses a store through a mapping"
    );
    check!(
        pread_ok(fdb, 4, 3)? == data[3..7],
        "an untouched range changed"
    );
    // write(2) shows in both mappings.
    check!(pwrite(fdb, b"by write", PAGE + 40) == 8, "pwrite");
    check!(
        a.load(BASE + PAGE + 40, 8)? == b"by write",
        "A misses write(2)"
    );
    // A store beside the write keeps both (one page, two writers).
    b.store(vb + PAGE + 48, b"+store")?;
    check!(
        pread_ok(fda, 14, PAGE + 40)? == b"by write+store",
        "a write(2) and a store on one page disagree"
    );
    // msync: (the stores and the write are all on the file's pages).
    a.enter();
    check!(msync(BASE, 2 * PAGE, MS_SYNC) == 0, "msync");
    check!(
        msync(BASE + 3, PAGE, MS_SYNC) == errno(EINVAL),
        "msync of an unaligned address"
    );
    check!(
        msync(BASE, 8 * PAGE, MS_SYNC) == errno(12),
        "msync of a range with a hole"
    );
    let size_before = fstat_size(fda)?;
    check!(size_before == 2 * PAGE, "size changed to {size_before}");
    // Unmap both: the data is in the file.
    a.enter();
    check!(munmap(BASE, 2 * PAGE) == 0, "munmap A");
    b.enter();
    check!(munmap(vb, 2 * PAGE) == 0, "munmap B");
    close(fda);
    close(fdb);
    let on_disk = bytes_of(&path)?;
    check!(on_disk[100..111] == *b"stored by A", "the store was lost");
    check!(
        on_disk[PAGE as usize + 40..PAGE as usize + 54] == *b"by write+store",
        "the second page was lost"
    );
    check!(on_disk.len() == data.len(), "the file changed size");
    Ok(())
}

pub fn shared_survives_remount() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/persist", &pattern(PAGE as usize, 1))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw("/data/persist")?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.store(BASE + 7, b"through a mapping")?;
    // No msync, no munmap: the address space dies, as at exit.
    close(fd);
    drop(space);
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync");
    data.remount()?;
    let after = slurp("/data/persist")?;
    check!(
        after[7..24] == *b"through a mapping",
        "the store did not survive exit and a remount"
    );
    data.check_clean()
}

pub fn refusals_and_protection() -> Result<(), String> {
    on_both(refusals_on)
}

fn refusals_on(dir: &str) -> Result<(), String> {
    let path = format!("{dir}/refuse");
    put(&path, &pattern(PAGE as usize, 2))?;
    let space = Space::new()?;
    space.enter();
    let ro = open_ro(&path)?;
    let wo = open(&path, O_WRONLY);
    check!(wo < 64, "open O_WRONLY");
    let eacces = errno(EACCES);
    let einval = errno(EINVAL);
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, ro, 0) == eacces,
        "shared writable of a read-only descriptor"
    );
    check!(
        mmap(BASE, PAGE, PROT_READ, MAP_SHARED | MAP_FIXED, wo, 0) == eacces,
        "a write-only descriptor cannot be mapped"
    );
    check!(
        mmap(BASE, PAGE, PROT_READ, MAP_PRIVATE | MAP_FIXED, ro, 1) == einval,
        "an unaligned offset"
    );
    check!(
        mem::vma::find(space.table, BASE).is_none(),
        "a refused mapping left a VMA"
    );
    // A read-only shared mapping can be read but not made writable.
    check!(
        mmap(BASE, PAGE, PROT_READ, MAP_SHARED | MAP_FIXED, ro, 0) == BASE,
        "read-only shared mapping"
    );
    check!(
        mprotect(BASE, PAGE, RW) == eacces,
        "mprotect(PROT_WRITE) of a shared mapping of a read-only fd"
    );
    check!(
        !space.can_touch(BASE, true),
        "a store to a read-only mapping was allowed"
    );
    check!(munmap(BASE, PAGE) == 0, "munmap");
    // PROT_NONE maps nothing; mprotect opens it for the next touch.
    check!(
        mmap(BASE, PAGE, PROT_NONE, MAP_PRIVATE | MAP_FIXED, ro, 0) == BASE,
        "PROT_NONE mapping"
    );
    check!(!space.can_touch(BASE, false), "PROT_NONE page was mapped");
    check!(mprotect(BASE, PAGE, PROT_READ) == 0, "mprotect");
    check!(
        space.can_touch(BASE, false),
        "page not faulted after mprotect"
    );
    check!(
        space.load(BASE, 4)? == pattern(4, 2),
        "wrong bytes after mprotect"
    );
    check!(munmap(BASE, PAGE) == 0, "munmap 2");
    // A directory, a pipe and a shared mapping of a snapshot are ENODEV.
    let (r, w) = {
        let mut fds = [0i32; 2];
        check!(
            syscall(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0, 0) == 0,
            "pipe"
        );
        (fds[0] as u64, fds[1] as u64)
    };
    check!(
        mmap(BASE, PAGE, PROT_READ, MAP_SHARED | MAP_FIXED, r, 0) == errno(ENODEV_),
        "a pipe"
    );
    close(r);
    close(w);
    close(ro);
    close(wo);
    Ok(())
}
