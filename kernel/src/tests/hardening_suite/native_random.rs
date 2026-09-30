//! Native entropy syscall 26 (networking plan N2): correctness, hostile
//! pointers and counts, statistics, and a soak.

use super::*;

const SYS_RANDOM: u64 = 26;

fn random(buffer: u64, len: u64) -> u64 {
    process::dispatch_for_test(SYS_RANDOM, buffer, len, 0)
}

/// Fill `[addr, addr + len)` (mapped scratch memory) with `value`.
fn fill(addr: u64, len: usize, value: u8) {
    // Safety: callers pass ranges inside the scratch pages `in_space` mapped.
    unsafe { core::ptr::write_bytes(addr as *mut u8, value, len) };
}

fn bytes<'a>(addr: u64, len: usize) -> &'a [u8] {
    // Safety: callers pass ranges inside the scratch pages `in_space` mapped.
    unsafe { core::slice::from_raw_parts(addr as *const u8, len) }
}

/// The count is honoured up to the cap and longer requests are short reads;
/// nothing past the count is written; zero is a no-op.
pub fn native_random_honours_counts_and_the_cap() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    in_space(|| {
        for len in [1u64, 2, 17, 32, 255, 256] {
            fill(SPACE, 0x1000, 0xEE);
            check!(random(SPACE, len) == len, "a {len}-byte request");
            check!(
                bytes(SPACE + len, 0x1000 - len as usize)
                    .iter()
                    .all(|b| *b == 0xEE),
                "a {len}-byte request wrote past its count"
            );
        }
        for len in [257u64, 4096, 1 << 32, u64::MAX] {
            fill(SPACE, 0x1000, 0xEE);
            check!(random(SPACE, len) == 256, "a {len:#x}-byte request");
            check!(
                bytes(SPACE + 256, 0x1000 - 256).iter().all(|b| *b == 0xEE),
                "a {len:#x}-byte request wrote past the cap"
            );
        }
        fill(SPACE, 64, 0xEE);
        check!(random(SPACE, 0) == 0, "a zero-length request");
        check!(
            bytes(SPACE, 64).iter().all(|b| *b == 0xEE),
            "a zero-length request wrote"
        );
        // 256 random bytes are not all one value.
        check!(random(SPACE, 256) == 256, "a full block");
        let block = bytes(SPACE, 256);
        check!(
            block.iter().any(|b| *b != block[0]),
            "a full block of identical bytes"
        );
        Ok(())
    })
}

/// A destination that cannot be written is `-EFAULT` with nothing written, for
/// every pointer the hostile-pointer tests use and every count; a zero count
/// never looks at the pointer.
pub fn native_random_rejects_bad_buffers() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    for buffer in [
        0xdead_0000u64,
        0,
        u64::MAX - 7,
        0xffff_8000_0000_0000,
        0x0000_7fff_ffff_f000,
    ] {
        for len in [1u64, 256, 4096, u64::MAX] {
            let code = random(buffer, len);
            check!(
                code == failed(EFAULT),
                "random({buffer:#x}, {len:#x}) -> {code:#x}"
            );
        }
        check!(
            random(buffer, 0) == 0,
            "a zero count touched the pointer {buffer:#x}"
        );
    }
    // A range that starts in mapped memory and runs off its end is refused
    // whole, not half-written.
    in_space(|| {
        let edge = SPACE + SPACE_PAGES * 4096 - 16;
        fill(edge, 16, 0xEE);
        check!(
            random(edge, 64) == failed(EFAULT),
            "a range running off the mapping"
        );
        check!(
            bytes(edge, 16).iter().all(|b| *b == 0xEE),
            "a refused call wrote part of the range"
        );
        Ok(())
    })
}

/// Every call is fresh (the pool never repeats), and 64 KiB of output is
/// balanced in bits and bytes.
pub fn native_random_is_unique_and_balanced() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    in_space(|| {
        let mut seen: Vec<[u8; 32]> = Vec::new();
        for _ in 0..1000 {
            check!(random(SPACE, 32) == 32, "a 32-byte request");
            let mut one = [0u8; 32];
            one.copy_from_slice(bytes(SPACE, 32));
            seen.push(one);
        }
        seen.sort_unstable();
        check!(
            seen.windows(2).all(|p| p[0] != p[1]),
            "two calls returned identical bytes"
        );

        let mut all = Vec::with_capacity(64 * 1024);
        while all.len() < 64 * 1024 {
            check!(random(SPACE, 256) == 256, "a full block");
            all.extend_from_slice(bytes(SPACE, 256));
        }
        let ones: u64 = all.iter().map(|b| u64::from(b.count_ones())).sum();
        let total = (all.len() * 8) as u64;
        check!(
            ones.abs_diff(total / 2) < total / 100,
            "bit balance off: {ones} ones of {total}"
        );
        let mut histogram = [0u32; 256];
        for &b in &all {
            histogram[b as usize] += 1;
        }
        check!(
            histogram.iter().all(|&n| n.abs_diff(256) < 100),
            "the byte histogram is skewed"
        );
        let mut blocks: Vec<&[u8]> = all.chunks_exact(16).collect();
        blocks.sort_unstable();
        check!(
            blocks.windows(2).all(|p| p[0] != p[1]),
            "a repeated 16-byte block"
        );
        Ok(())
    })
}

/// Soak: a hundred thousand calls of mixed sizes and hostile counts never
/// fault, never return a wrong count and never repeat an 8-byte prefix more
/// than chance allows.
pub fn native_random_soak() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    in_space(|| {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut prefixes: Vec<u64> = Vec::new();
        for round in 0..100_000u32 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let len = match round % 5 {
                0 => seed >> 40,
                1 => 1 + seed % 256,
                2 => u64::MAX - (seed & 0xFF),
                _ => 8,
            };
            let want = len.min(256);
            let code = random(SPACE, len);
            check!(
                code == want,
                "round {round}: random(len {len:#x}) -> {code:#x}, want {want:#x}"
            );
            if want >= 8 && round % 7 == 0 {
                let mut head = [0u8; 8];
                head.copy_from_slice(bytes(SPACE, 8));
                prefixes.push(u64::from_le_bytes(head));
            }
        }
        prefixes.sort_unstable();
        let repeats = prefixes.windows(2).filter(|p| p[0] == p[1]).count();
        check!(repeats == 0, "{repeats} repeated 64-bit prefixes");
        Ok(())
    })
}
