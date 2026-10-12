//! `memfd_create` and its file operations, and the `mmap` requests that are
//! refused.

use super::*;

/// Creation, flags, the file operations, sparse growth and shrink, and the
/// `/proc/self/fd` link text.
pub fn memfd_file_operations() -> Result<(), String> {
    fresh()?;
    let pages = live_pages();
    check!(
        memfd_create("x", 0x80) == EINVAL,
        "an unknown flag was accepted"
    );
    check!(memfd_create("x", 4) == EINVAL, "MFD_HUGETLB was accepted");
    let long = "n".repeat(250);
    check!(memfd_create(&long, 0) == EINVAL, "a 250-byte name");
    let longest = "n".repeat(249);
    let fd = memfd_create(&longest, 0);
    check!((fd as i64) > 2, "a 249-byte name: {fd:#x}");
    close_fd(fd);
    check!(
        sys_checked(MEMFD_CREATE, &[8, 0]) == EFAULT,
        "a bad name pointer was accepted"
    );

    let plain = memfd_create("plain", 0);
    check!((plain as i64) > 2, "memfd_create returned {plain:#x}");
    check!(
        task::fd_kind(plain as usize) == task::FdKind::Memfd && !task::fd_cloexec(plain as usize),
        "kind or close-on-exec"
    );
    let fd = memfd_create("scratch", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    check!(task::fd_cloexec(fd as usize), "MFD_CLOEXEC was ignored");

    // fstat: a regular file of mode 0777.
    let mut stat = [0u8; 144];
    check!(sys(5, &[fd, stat.as_mut_ptr() as u64]) == 0, "fstat");
    let mode = u32::from_le_bytes(stat[24..28].try_into().unwrap());
    let size = u64::from_le_bytes(stat[48..56].try_into().unwrap());
    check!(mode == 0o100777 && size == 0, "mode {mode:#o}, size {size}");

    // read, write, offsets.
    check!(write_fd(fd, b"hello") == 5, "write");
    check!(sys(8, &[fd, 0, SEEK_CUR]) == 5, "the offset after a write");
    check!(sys(8, &[fd, 0, SEEK_SET]) == 0, "rewind");
    check!(read_fd(fd, 16) == b"hello", "read back");
    check!(read_fd(fd, 16).is_empty(), "no end-of-file");
    check!(sys(8, &[fd, 0, SEEK_END]) == 5, "SEEK_END");
    check!(
        sys(8, &[fd, (-1i64) as u64, SEEK_SET]) == EINVAL,
        "negative seek"
    );

    // A duplicate shares the open file description, and so the offset.
    let dup = sys(32, &[fd]);
    check!(sys(8, &[fd, 10, SEEK_SET]) == 10, "seek");
    check!(
        sys(8, &[dup, 0, SEEK_CUR]) == 10,
        "dup does not share the offset"
    );

    // Sparse growth: a hole reads as zeros and costs no page.
    let mem = memfd_of(fd)?;
    check!(sys(77, &[fd, 3 * PAGE]) == 0, "ftruncate up");
    check!(
        mem.size() == 3 * PAGE && mem.resident_pages() == 1,
        "sparse growth"
    );
    check!(
        pread_fd(fd, 8, PAGE + 100) == [0u8; 8],
        "a hole did not read as zeros"
    );
    check!(mem.resident_pages() == 1, "reading a hole made a page");
    check!(
        sys(18, &[fd, b"tail".as_ptr() as u64, 4, 2 * PAGE + 4094]) == 4,
        "pwrite across the end of the file's last pages"
    );
    check!(
        mem.resident_pages() == 3,
        "pwrite across two pages: {}",
        mem.resident_pages()
    );
    check!(
        pread_fd(fd, 4, 2 * PAGE + 4094) == b"tail",
        "pwrite did not read back across the page boundary"
    );
    check!(
        mem.size() == 3 * PAGE + 2,
        "a write past the end grows the file"
    );

    // Shrink drops the pages and zeroes the kept page's tail.
    check!(sys(77, &[fd, 3]) == 0, "ftruncate down");
    check!(mem.resident_pages() == 1 && mem.size() == 3, "shrink");
    check!(sys(77, &[fd, 100]) == 0, "ftruncate up again");
    check!(
        pread_fd(fd, 6, 0) == [b'h', b'e', b'l', 0, 0, 0],
        "bytes past the shrunken end survived: {:?}",
        pread_fd(fd, 6, 0)
    );
    check!(sys(77, &[fd, 1 << 50]) == EFBIG, "an absurd size");
    check!(sys(77, &[fd, (-1i64) as u64]) == EINVAL, "a negative size");

    // /proc/self/fd/N.
    let link = cpath(&format!("/proc/self/fd/{fd}"));
    let mut text = [0u8; 64];
    let n = sys(89, &[link.as_ptr() as u64, text.as_mut_ptr() as u64, 64]);
    check!(
        (n as i64) > 0 && &text[..n as usize] == b"/memfd:scratch (deleted)",
        "readlink gave {:?}",
        &text[..(n as usize).min(64)]
    );

    for fd in [plain, fd, dup] {
        check!(sys(3, &[fd]) == 0, "close {fd}");
    }
    drop(mem);
    check!(
        live_pages() == pages,
        "pages leaked: {} -> {}",
        pages,
        live_pages()
    );
    Ok(())
}

/// Bad requests are refused rather than ignored or treated as private.
pub fn memfd_mmap_refusals() -> Result<(), String> {
    fresh()?;
    let a = Space::new()?;
    a.enter();
    let fd = memfd_create("refuse", 0);
    check!(sys(77, &[fd, 4 * PAGE]) == 0, "ftruncate");
    let rw = PROT_READ | PROT_WRITE;
    check!(
        mmap(BASE, PAGE, rw, MAP_FIXED, fd, 0) == EINVAL,
        "neither SHARED nor PRIVATE"
    );
    check!(
        mmap(BASE, PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 100) == EINVAL,
        "unaligned offset"
    );
    check!(
        mmap(BASE, 0, rw, MAP_SHARED | MAP_FIXED, fd, 0) == EINVAL,
        "zero length"
    );
    check!(
        mmap(BASE, PAGE, PROT_NONE, MAP_SHARED | MAP_FIXED, fd, 0) == EINVAL,
        "a PROT_NONE shared mapping"
    );
    check!(
        mmap(
            BASE,
            PAGE,
            PROT_NONE,
            MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED,
            !0,
            0
        ) == EINVAL,
        "a PROT_NONE shared anonymous mapping"
    );
    check!(
        mmap(BASE, PAGE, rw, MAP_SHARED | MAP_FIXED, 99, 0) == EBADF,
        "a closed descriptor"
    );
    let (r, _w) = pipe()?;
    check!(
        mmap(BASE, PAGE, rw, MAP_SHARED | MAP_FIXED, r, 0) == ENODEV,
        "a pipe"
    );
    let file = cpath("/tmp/regular");
    let opened = sys(2, &[file.as_ptr() as u64, 0o102, 0o600]);
    check!((opened as i64) > 2, "open");
    check!(
        mmap(BASE, PAGE, rw, MAP_SHARED | MAP_FIXED, opened, 0) == ENODEV,
        "MAP_SHARED of a regular file was not refused"
    );
    check!(
        mem::vma::find(a.table, BASE).is_none(),
        "a refused mapping left a VMA"
    );
    // More than the machine has is refused up front, not filled and undone.
    let enormous = 1u64 << 44;
    check!(
        mmap(0, enormous, rw, MAP_SHARED | MAP_ANONYMOUS, !0, 0) == ENOMEM,
        "an enormous shared anonymous mapping"
    );
    check!(
        mmap(0, enormous, rw, MAP_SHARED, fd, 0) == ENOMEM,
        "an enormous memfd mapping"
    );
    check!(
        mmap(0, enormous, rw, MAP_PRIVATE, fd, 0) == ENOMEM,
        "an enormous private memfd mapping"
    );
    // Mapping past the end of the file works (pages appear on demand) and
    // survives a later growth.
    check!(
        mmap(BASE, 8 * PAGE, rw, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mapping past the end"
    );
    check!(sys(77, &[fd, 8 * PAGE]) == 0, "grow after mapping");
    a.store(BASE + 7 * PAGE, b"far")?;
    check!(
        pread_fd(fd, 3, 7 * PAGE) == b"far",
        "a store beyond the old end"
    );
    check!(munmap(BASE, 8 * PAGE) == 0, "munmap");
    for fd in [fd, opened, r] {
        close_fd(fd);
    }
    Ok(())
}
