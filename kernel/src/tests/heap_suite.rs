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

pub(super) const CASES: &[(&str, Test)] = &[("heap_vec_integrity", vec_integrity)];
