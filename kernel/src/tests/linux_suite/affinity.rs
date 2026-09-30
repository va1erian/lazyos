//! `sched_getaffinity(2)`: the argument order the dispatcher honours and the
//! one-CPU mask it reports. A C++/Zig runtime (`std.Thread.getCpuCount`) counts
//! the mask's bits and divides by the result, so a mask that is never written
//! is a divide error at process start (found by the xui-litehtml spike).

use super::*;

const SYS_SCHED_GETAFFINITY: u64 = 204;

/// `sched_getaffinity(pid 0, len, mask)`.
fn getaffinity(len: usize, mask: &mut [u8]) -> u64 {
    process::linux::dispatch_for_test(
        SYS_SCHED_GETAFFINITY,
        0,
        len as u64,
        mask.as_mut_ptr() as u64,
    )
}

/// A full-size (glibc `cpu_set_t`) request gets 8 valid bytes: CPU 0 only.
pub fn sched_getaffinity_reports_one_cpu() -> Result<(), String> {
    fresh()?;
    let mut mask = [0xAAu8; 128];
    let got = getaffinity(mask.len(), &mut mask);
    check!(got == 8, "returned {got}, want the 8 bytes written");
    check!(
        u64::from_le_bytes(mask[..8].try_into().unwrap()) == 1,
        "mask word is {:?}, want CPU 0 only",
        &mask[..8]
    );
    check!(
        mask[8..].iter().all(|&b| b == 0xAA),
        "bytes past the returned length were touched"
    );
    Ok(())
}

/// A sub-word buffer gets one byte (CPU 0) and no more; empty is 0.
pub fn sched_getaffinity_short_buffers() -> Result<(), String> {
    fresh()?;
    let mut one = [0xAAu8; 4];
    check!(getaffinity(1, &mut one) == 1, "one-byte request");
    check!(one == [1, 0xAA, 0xAA, 0xAA], "one-byte mask: {one:?}");
    let mut none = [0xAAu8; 4];
    check!(getaffinity(0, &mut none) == 0, "zero-length request");
    check!(none == [0xAA; 4], "zero-length request wrote: {none:?}");
    Ok(())
}

/// Many varied requests never write past the length asked for.
pub fn sched_getaffinity_soak_bounds() -> Result<(), String> {
    fresh()?;
    for round in 0..5000usize {
        let len = 1 + round % 200;
        let mut buf = vec![0x5Au8; len + 16];
        let got = getaffinity(len, &mut buf[..len]) as usize;
        // A request of 8+ bytes gets the whole word; a shorter one gets byte 0.
        let want = if len >= 8 { 8 } else { 1 };
        check!(got == want, "round {round}: len {len} returned {got}");
        check!(
            buf[len..].iter().all(|&b| b == 0x5A),
            "round {round}: overrun"
        );
        check!(buf[0] == 1, "round {round}: CPU 0 missing");
    }
    Ok(())
}
