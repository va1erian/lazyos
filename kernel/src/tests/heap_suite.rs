//! Kernel heap allocator correctness.

use super::*;

/// Allocating, filling, dropping and reusing heap blocks keeps data intact.
pub fn vec_integrity() -> Result<(), String> {
    let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(64);
    for round in 0..64u32 {
        let mut buffer = Vec::with_capacity(4096);
        for index in 0..4096u32 {
            buffer.push((round as u8) ^ (index as u8).wrapping_mul(7));
        }
        buffers.push(buffer);
    }
    for (round, buffer) in buffers.iter().enumerate() {
        check!(
            buffer.len() == 4096,
            "round {round} length is {}",
            buffer.len()
        );
        for (index, &byte) in buffer.iter().enumerate() {
            check!(
                byte == (round as u8) ^ (index as u8).wrapping_mul(7),
                "round {round} byte {index} is {byte:#x} (heap corruption?)"
            );
        }
    }
    drop(buffers);

    // Freed blocks should be reusable and still hold what we write.
    let mut reused: Vec<u8> = Vec::with_capacity(4096);
    for index in 0..4096u32 {
        reused.push((index as u8) ^ 0x5a);
    }
    check!(
        reused.len() == 4096,
        "reallocated length is {}",
        reused.len()
    );
    check!(
        reused[2048] == (2048u32 as u8) ^ 0x5a,
        "reallocated buffer is corrupted"
    );
    Ok(())
}

/// An allocation larger than the heap's free space grows the heap (mapping
/// fresh frames at its top) instead of failing, and the memory is usable.
pub fn grows_on_demand() -> Result<(), String> {
    let before = mem::heap_stats();
    let want = before.free + (8 << 20);
    let mut big: Vec<u8> = Vec::new();
    big.try_reserve_exact(want)
        .map_err(|_| format!("could not grow the heap by {want} bytes: {before:?}"))?;
    big.resize(want, 0xa5);
    check!(
        big[want - 1] == 0xa5 && big[0] == 0xa5,
        "grown memory is not usable"
    );
    let after = mem::heap_stats();
    check!(
        after.total > before.total && after.growths > before.growths,
        "the heap did not grow: {before:?} -> {after:?}"
    );
    check!(
        after.total <= after.max,
        "heap above its ceiling: {after:?}"
    );
    drop(big);
    Ok(())
}

/// At its ceiling the heap refuses: a fallible allocation fails cleanly, the
/// refusal is counted, and the heap keeps working.
pub fn refuses_past_ceiling() -> Result<(), String> {
    use crate::limits::{self, Id};
    let total = mem::heap_stats().total as u64;
    limits::set_for_test(Id::HeapMax, total);
    let before = mem::heap_stats();
    let mut huge: Vec<u8> = Vec::new();
    let refused = huge.try_reserve_exact(before.free + (64 << 20)).is_err();
    let after = mem::heap_stats();
    let small = alloc::vec![7u8; 4096];
    limits::reset_for_test();
    check!(refused, "an allocation past the ceiling succeeded");
    check!(
        after.total == before.total,
        "the heap grew past its ceiling"
    );
    check!(
        after.grow_failures > before.grow_failures,
        "the refusal was not counted"
    );
    check!(small[4095] == 7, "the heap broke after a refusal");
    Ok(())
}

/// Soak: allocate and free blocks from 16 bytes to 6 MiB in a shuffled
/// order, growing the heap repeatedly; data stays intact and everything is
/// returned (used bytes back to where they started).
pub fn growth_soak() -> Result<(), String> {
    let start = mem::heap_stats().used;
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for round in 0..40u64 {
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        for index in 0..24u64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let len = 16usize << (seed % 19);
            let mut block = Vec::new();
            block
                .try_reserve_exact(len)
                .map_err(|_| format!("round {round}: {len} bytes refused"))?;
            block.resize(len, (round ^ index) as u8);
            blocks.push(block);
        }
        for (index, block) in blocks.iter().enumerate() {
            let want = (round ^ index as u64) as u8;
            check!(
                block[0] == want && block[block.len() - 1] == want,
                "round {round}: block {index} corrupted"
            );
        }
    }
    let end = mem::heap_stats().used;
    check!(end <= start + 4096, "heap use grew from {start} to {end}");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("heap_vec_integrity", vec_integrity),
    ("heap_grows_on_demand", grows_on_demand),
    ("heap_refuses_past_ceiling", refuses_past_ceiling),
    ("heap_growth_soak", growth_soak),
];
