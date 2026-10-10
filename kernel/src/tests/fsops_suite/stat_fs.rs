//! `stat` (15) with `STAT_FS`: the total and free bytes of a path's
//! filesystem, which `pkgd` reports after provisioning (issue #703). Split
//! out of `fsops_suite.rs`.

use super::*;
use crate::process::fsops::STAT_FS;
use crate::tests::hardening_suite::in_space;

/// `[total, free]` bytes of the filesystem holding `path`.
fn space(path: &str) -> Result<(u64, u64), u64> {
    let path = cstr(path);
    let mut out = [0u64; 2];
    match call(STAT, path.as_ptr() as u64, out.as_mut_ptr() as u64, STAT_FS) {
        0 => Ok((out[0], out[1])),
        code => Err(code),
    }
}

/// Writing a file takes its bytes from the free figure and deleting it gives
/// them back; the total stays; files and directories of one filesystem agree;
/// a missing path, an unknown flag and bad result pointers are refused.
pub(super) fn stat_fs_semantics() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/sfs/f", "/tmp/sfs"]);
    check!(path_call(MKDIR, "/tmp/sfs") == 0, "mkdir");
    let (total, free) = space("/tmp/sfs").map_err(|e| format!("statfs /tmp/sfs: {e:#x}"))?;
    check!(total > 0 && free <= total, "total {total} free {free}");
    check!(
        space("/tmp") == Ok((total, free)),
        "/tmp and /tmp/sfs differ"
    );
    let data = vec![0x5Au8; 256 * 1024];
    check!(put("/tmp/sfs/f", &data) == data.len() as u64, "write");
    let (total_after, free_after) = space("/tmp/sfs/f").map_err(|e| format!("{e:#x}"))?;
    check!(
        total_after == total,
        "the total moved: {total} -> {total_after}"
    );
    check!(
        free_after + data.len() as u64 <= free,
        "a 256 KiB file took only {} bytes",
        free - free_after
    );
    check!(path_call(UNLINK, "/tmp/sfs/f") == 0, "unlink");
    check!(
        space("/tmp/sfs") == Ok((total, free)),
        "the space did not come back"
    );
    // The figure is the filesystem's, not the stat of the path.
    check!(stat("/tmp/sfs").is_ok(), "plain stat broke");
    check!(
        space("/tmp/sfs/missing") == Err(failed(ENOENT)),
        "a missing path has a filesystem"
    );
    let path = cstr("/tmp/sfs");
    let mut out = [0u64; 2];
    check!(
        call(STAT, path.as_ptr() as u64, out.as_mut_ptr() as u64, 2) == failed(EINVAL),
        "an unknown flag was accepted"
    );
    // `/` is the boot filesystem, with its own figures.
    check!(space("/").is_ok(), "statfs / failed");
    strict(|| in_space(bad_pointers))
}

/// With validation on and the path in real user pages: a null or kernel
/// result pointer is `EFAULT` (the kernel buffer untouched), and the same
/// call with a user result pointer succeeds, proving the setup works.
fn bad_pointers() -> Result<(), String> {
    use crate::tests::hardening_suite::SPACE;
    let (path, out) = (SPACE, SPACE + 0x100);
    // SAFETY: `in_space` maps `SPACE` writable while this runs; the path
    // fits in its first page.
    unsafe { core::ptr::copy_nonoverlapping(b"/tmp/sfs\0".as_ptr(), path as *mut u8, 9) };
    check!(
        call(STAT, path, 0, STAT_FS) == failed(EFAULT),
        "a null result pointer was accepted"
    );
    let canary = [0xA5A5_A5A5_A5A5_A5A5u64; 2];
    check!(
        call(STAT, path, canary.as_ptr() as u64, STAT_FS) == failed(EFAULT),
        "a kernel result pointer was accepted"
    );
    check!(
        canary == [0xA5A5_A5A5_A5A5_A5A5; 2],
        "stat wrote kernel memory"
    );
    check!(
        call(STAT, path, out, STAT_FS) == 0,
        "a user result pointer was refused"
    );
    // SAFETY: `out` is inside the mapped `SPACE`; the call just wrote it.
    let words = unsafe { core::slice::from_raw_parts(out as *const u64, 2) };
    check!(words[0] > 0 && words[1] <= words[0], "figures {words:?}");
    Ok(())
}

/// Soak: many writes and deletes with a `STAT_FS` after each; the free
/// figure tracks them exactly and nothing leaks.
pub(super) fn soak_stat_fs() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/sfsoak/f", "/tmp/sfsoak"]);
    check!(path_call(MKDIR, "/tmp/sfsoak") == 0, "mkdir");
    let (_, empty) = space("/tmp/sfsoak").map_err(|e| format!("{e:#x}"))?;
    let before = mem::frame_stats().live();
    for round in 0..400usize {
        let len = 4096 * (1 + round % 16);
        check!(
            put("/tmp/sfsoak/f", &vec![round as u8; len]) == len as u64,
            "write {round}"
        );
        let (_, free) = space("/tmp/sfsoak/f").map_err(|e| format!("round {round}: {e:#x}"))?;
        check!(
            free + len as u64 <= empty,
            "round {round}: {len} bytes written, {} taken",
            empty - free
        );
        check!(path_call(UNLINK, "/tmp/sfsoak/f") == 0, "unlink {round}");
        check!(
            space("/tmp/sfsoak").map(|(_, free)| free) == Ok(empty),
            "round {round}: the space did not come back"
        );
    }
    let after = mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}
