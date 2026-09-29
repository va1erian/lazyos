//! Regressions for unchecked `i64`/`u64` arithmetic reachable from
//! userspace: `lseek` offset overflow (issue #225) and the `align_up`
//! overflow in the `mmap` family plus native `sbrk` (issue #224). Both
//! were kernel panics on the default profile, where overflow checks are
//! on and `panic = "abort"`.

use super::*;

const ENOMEM: i64 = 12;
const EINVAL: i64 = 22;

/// `lseek` positions may not overflow or go negative (`-EINVAL`), but a
/// legal position past EOF must stick instead of snapping back to EOF.
pub fn lseek_overflow_and_past_eof() -> Result<(), String> {
    fresh()?;
    let fd = task::fd_open(task::Fd::File {
        data: b"hello".to_vec(),
        offset: 0,
    })
    .ok_or("fd_open failed")?;

    // A negative absolute position is -EINVAL, not a silent clamp to 0.
    let code = process::linux::dispatch_for_test(8, fd as u64, (-1i64) as u64, 0);
    check!(
        code == failed(EINVAL),
        "lseek(SEEK_SET, -1) -> {code:#x}, expected -EINVAL"
    );

    // Park the offset high, then seek by i64::MAX: the sum overflows i64.
    let far = i64::MAX as u64;
    let code = process::linux::dispatch_for_test(8, fd as u64, far, 0);
    check!(code == far, "lseek(SEEK_SET, i64::MAX) -> {code:#x}");
    let code = process::linux::dispatch_for_test(8, fd as u64, far, 1);
    check!(
        code == failed(EINVAL),
        "lseek(SEEK_CUR, i64::MAX) overflowed -> {code:#x}, expected -EINVAL"
    );

    // A position past EOF is legal and is preserved across a failed read.
    let code = process::linux::dispatch_for_test(8, fd as u64, 100, 0);
    check!(code == 100, "lseek(SEEK_SET, 100) past EOF -> {code:#x}");
    check!(
        task::fd_offset(fd) == Some(100),
        "lseek past EOF did not stick: {:?}",
        task::fd_offset(fd)
    );
    let bytes = task::fd_read(fd, 8).ok_or("fd_read at EOF failed")?;
    check!(
        bytes.is_empty(),
        "read past EOF returned {} bytes",
        bytes.len()
    );
    check!(
        task::fd_offset(fd) == Some(100),
        "a read past EOF moved the offset to {:?}",
        task::fd_offset(fd)
    );

    // The descriptor is still usable after all of that.
    let code = process::linux::dispatch_for_test(8, fd as u64, 0, 0);
    check!(code == 0, "lseek(SEEK_SET, 0) -> {code:#x}");
    let bytes = task::fd_read(fd, 8).ok_or("fd_read failed")?;
    check!(bytes == b"hello", "file contents changed: {bytes:?}");

    let _ = task::fd_close(fd);
    Ok(())
}

/// Huge user lengths must be refused with the right errno rather than
/// overflowing the `align_up` addition in the `mmap` family or `sbrk`.
pub fn mmap_family_rejects_overflowing_lengths() -> Result<(), String> {
    fresh()?;

    // mmap(0, u64::MAX, RW, MAP_PRIVATE|MAP_ANONYMOUS): no page-rounded
    // length fits below MMAP_LIMIT.
    let code = process::linux::dispatch_args_for_test(9, 0, u64::MAX, 3, 0x02 | 0x20);
    check!(
        code == failed(ENOMEM),
        "mmap(len=u64::MAX) -> {code:#x}, expected -ENOMEM"
    );

    // munmap/mprotect: addr + len == u64::MAX still overflows the round-up.
    let code = process::linux::dispatch_for_test(11, 0, u64::MAX, 0);
    check!(
        code == failed(EINVAL),
        "munmap(len=u64::MAX) -> {code:#x}, expected -EINVAL"
    );
    let code = process::linux::dispatch_for_test(10, 0, u64::MAX, 3);
    check!(
        code == failed(EINVAL),
        "mprotect(len=u64::MAX) -> {code:#x}, expected -EINVAL"
    );

    // brk reports the unchanged break rather than panicking.
    let before = task::brk();
    let code = process::linux::dispatch_for_test(12, u64::MAX, 0, 0);
    check!(
        code == before,
        "brk(u64::MAX) -> {code:#x}, expected unchanged break {before:#x}"
    );

    // mremap: rounding new_size, and rounding a checked-but-unaligned
    // old_end == u64::MAX, both overflow.
    let code = process::linux::dispatch_args5_for_test(25, 0x0040_0000, 4096, u64::MAX, 1, 0);
    check!(
        code == failed(EINVAL),
        "mremap(new_size=u64::MAX) -> {code:#x}, expected -EINVAL"
    );
    let code = process::linux::dispatch_args5_for_test(25, u64::MAX - 0xFFF, 0xFFF, 4096, 1, 0);
    check!(
        code == failed(EINVAL),
        "mremap(old_end=u64::MAX) -> {code:#x}, expected -EINVAL"
    );

    // Native sbrk: the page round-up of a target just below u64::MAX wraps.
    let current = task::heap_break();
    let code = process::dispatch_for_test(4, u64::MAX - current, 0, 0);
    check!(
        code == u64::MAX,
        "sbrk(wrapping) -> {code:#x}, expected the failure sentinel"
    );

    Ok(())
}
