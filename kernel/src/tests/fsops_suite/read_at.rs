//! `read_at` (syscall 30), the bounded ranged read the package installer
//! streams big archives with. Split out of `fsops_suite.rs`.

use super::*;

const READ_AT: u64 = 30;

/// `read_at` (30): up to `len` bytes of `path` at `offset` into `buf`.
fn read_at(path: &str, buf: &mut [u8], offset: u64) -> u64 {
    let path = cstr(path);
    let request = [buf.as_mut_ptr() as u64, buf.len() as u64, offset];
    call(READ_AT, path.as_ptr() as u64, request.as_ptr() as u64, 0)
}

/// A file of `len` bytes whose byte `i` is `(i * 7 + i / 251) as u8`, so a
/// range read at the wrong offset cannot match by accident.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// Ranges, the end of the file, the per-call cap, and every refusal: a missing
/// file, a directory, a bad request pointer and a bad buffer pointer.
pub(super) fn read_at_semantics() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/ra/f", "/tmp/ra/big", "/tmp/ra"]);
    check!(path_call(MKDIR, "/tmp/ra") == 0, "mkdir");
    let data = pattern(5000);
    check!(put("/tmp/ra/f", &data) == 5000, "write");
    let mut buf = vec![0u8; 100];
    check!(read_at("/tmp/ra/f", &mut buf, 0) == 100, "first range");
    check!(buf[..] == data[..100], "first range differs");
    check!(read_at("/tmp/ra/f", &mut buf, 4321) == 100, "middle range");
    check!(buf[..] == data[4321..4421], "middle range differs");
    check!(read_at("/tmp/ra/f", &mut buf, 4950) == 50, "short tail");
    check!(buf[..50] == data[4950..], "tail differs");
    check!(read_at("/tmp/ra/f", &mut buf, 5000) == 0, "read at EOF");
    check!(
        read_at("/tmp/ra/f", &mut buf, u64::MAX) == 0,
        "read far past EOF"
    );
    check!(read_at("/tmp/ra/f", &mut [], 10) == 0, "an empty read");
    check!(
        read_at("/tmp/ra/missing", &mut buf, 0) == failed(ENOENT),
        "read a missing file"
    );
    check!(
        read_at("/tmp/ra", &mut buf, 0) == failed(21),
        "read a directory"
    );
    // One call never returns more than MAX_WRITE, whatever `len` says.
    let cap = process::fsops::MAX_WRITE as usize;
    let big = pattern(cap + 4096);
    check!(put("/tmp/ra/big", &big[..cap]) == cap as u64, "write big");
    let path = cstr("/tmp/ra/big");
    check!(
        call(28, path.as_ptr() as u64, big[cap..].as_ptr() as u64, 4096) == 4096,
        "append big tail"
    );
    let mut whole = vec![0u8; cap + 4096];
    check!(
        read_at("/tmp/ra/big", &mut whole, 0) == cap as u64,
        "capped read"
    );
    check!(whole[..cap] == big[..cap], "capped read differs");
    check!(
        read_at("/tmp/ra/big", &mut whole, cap as u64) == 4096,
        "after the cap"
    );
    check!(whole[..4096] == big[cap..], "data after the cap differs");
    // Bad pointers are EFAULT: the request, then the destination buffer.
    strict(|| -> Result<(), String> {
        let path = cstr("/tmp/ra/f");
        check!(
            call(READ_AT, path.as_ptr() as u64, 0, 0) == failed(EFAULT),
            "a null request was accepted"
        );
        let request = [0xFFFF_8000_0000_0000u64, 16, 0];
        check!(
            call(READ_AT, path.as_ptr() as u64, request.as_ptr() as u64, 0) == failed(EFAULT),
            "read_at wrote kernel memory"
        );
        Ok(())
    })?;
    Ok(())
}

/// Soak: stream a multi-chunk file many times in odd-sized ranges; every pass
/// reassembles it exactly and no frames leak.
pub(super) fn soak_read_at_stream() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/rasoak/f", "/tmp/rasoak"]);
    check!(path_call(MKDIR, "/tmp/rasoak") == 0, "mkdir");
    let data = pattern(300_000);
    check!(put("/tmp/rasoak/f", &data) == data.len() as u64, "write");
    let before = mem::frame_stats().live();
    let mut chunk = vec![0u8; 70_001];
    for pass in 0..60usize {
        // 1 KiB..70 KiB: odd sizes that straddle FS block boundaries.
        let step = 1_021 + (pass * 9_973) % (chunk.len() - 1_021);
        let mut offset = 0usize;
        while offset < data.len() {
            let n = read_at("/tmp/rasoak/f", &mut chunk[..step], offset as u64) as usize;
            let expect = step.min(data.len() - offset);
            check!(
                n == expect,
                "pass {pass} at {offset}: read {n}, wanted {expect}"
            );
            check!(
                chunk[..n] == data[offset..offset + n],
                "pass {pass} at {offset}: bytes differ"
            );
            offset += n;
        }
    }
    let after = mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}
