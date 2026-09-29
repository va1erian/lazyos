//! `getrandom(2)` and the kernel CSPRNG (issue #232): RFC 8439 known answer,
//! uniqueness within a single tick, statistical sanity, periodic reseed, soak.

use super::*;
use crate::entropy;

const SYS_GETRANDOM: u64 = 318;

fn getrandom(buf: &mut [u8]) -> u64 {
    process::linux::dispatch_for_test(SYS_GETRANDOM, buf.as_mut_ptr() as u64, buf.len() as u64, 0)
}

/// RFC 8439 section 2.3.2: the ChaCha20 block function.
pub fn chacha20_rfc8439_block() -> Result<(), String> {
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let nonce = [0, 0, 0, 9, 0, 0, 0, 0x4a, 0, 0, 0, 0];
    let block = entropy::chacha20_block(&key, 1, &nonce);
    let want: [u32; 16] = [
        0xe4e7f110, 0x15593bd1, 0x1fdd0f50, 0xc47120a3, 0xc7f4d1c7, 0x0368c033, 0x9aaa2204,
        0x4e6cd4c3, 0x466482d2, 0x09aa9f07, 0x05d7c214, 0xa2028bd9, 0xd19c12b5, 0xb94e16de,
        0xe883d0cb, 0x4e3c50a2,
    ];
    for (i, (chunk, want)) in block.chunks_exact(4).zip(want).enumerate() {
        let got = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        check!(got == want, "word {i}: {got:#010x}, want {want:#010x}");
    }
    Ok(())
}

/// Back-to-back requests (same tick) never repeat; the length is honoured.
pub fn getrandom_unique_within_tick() -> Result<(), String> {
    fresh()?;
    let mut seen: Vec<[u8; 32]> = Vec::new();
    for _ in 0..1000 {
        let mut out = [0u8; 32];
        check!(
            getrandom(&mut out) == 32,
            "getrandom returned a short count"
        );
        seen.push(out);
    }
    seen.sort_unstable();
    check!(
        seen.windows(2).all(|pair| pair[0] != pair[1]),
        "two getrandom calls returned identical bytes"
    );
    let mut none: [u8; 0] = [];
    check!(getrandom(&mut none) == 0, "zero-length getrandom");
    let mut odd = vec![0u8; 1001];
    check!(getrandom(&mut odd) == 1001, "odd-length getrandom");
    check!(odd.iter().any(|&b| b != 0), "odd-length output is all zero");
    Ok(())
}

/// Bit balance and block uniqueness over 64 KiB.
pub fn getrandom_statistics() -> Result<(), String> {
    fresh()?;
    let mut buf = vec![0u8; 64 * 1024];
    // The kernel caps one call (a short read is legal), so loop like a caller.
    let mut filled = 0;
    while filled < buf.len() {
        let got = getrandom(&mut buf[filled..]) as usize;
        check!(got > 0 && got <= buf.len() - filled, "bad count {got}");
        filled += got;
    }
    let ones: u64 = buf.iter().map(|b| u64::from(b.count_ones())).sum();
    let total = (buf.len() * 8) as u64;
    check!(
        ones.abs_diff(total / 2) < total / 100,
        "bit balance off: {ones} ones of {total}"
    );
    let mut histogram = [0u32; 256];
    for &b in &buf {
        histogram[b as usize] += 1;
    }
    // Expected 256 per value; sigma is 16, so 100 is over 6 sigma.
    check!(
        histogram.iter().all(|&n| n.abs_diff(256) < 100),
        "byte histogram is skewed"
    );
    let mut blocks: Vec<&[u8]> = buf.chunks_exact(16).collect();
    blocks.sort_unstable();
    check!(
        blocks.windows(2).all(|p| p[0] != p[1]),
        "repeated 16-byte block"
    );
    Ok(())
}

/// The pool re-keys with fresh entropy periodically and on demand.
pub fn entropy_reseeds() -> Result<(), String> {
    fresh()?;
    let mut out = [0u8; 16];
    getrandom(&mut out);
    let before = entropy::reseed_count();
    for _ in 0..300 {
        getrandom(&mut out);
    }
    check!(
        entropy::reseed_count() > before,
        "no reseed after 300 requests"
    );
    let before = entropy::reseed_count();
    entropy::force_reseed();
    check!(
        entropy::reseed_count() == before + 1,
        "forced reseed not counted"
    );
    Ok(())
}

/// Soak: many small and large requests stay unique and never fault.
pub fn getrandom_soak() -> Result<(), String> {
    fresh()?;
    let mut previous = vec![0u8; 4096];
    let mut current = vec![0u8; 4096];
    check!(getrandom(&mut previous) == 4096, "short count");
    for i in 0..500 {
        check!(getrandom(&mut current) == 4096, "short count at {i}");
        check!(current != previous, "identical 4 KiB outputs at {i}");
        core::mem::swap(&mut previous, &mut current);
    }
    let mut small = [0u8; 8];
    let mut last = [0u8; 8];
    for i in 0..20_000 {
        getrandom(&mut small);
        check!(small != last, "identical 8-byte outputs at {i}");
        last = small;
    }
    Ok(())
}
