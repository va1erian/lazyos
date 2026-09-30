//! Address-space reuse for anonymous `mmap`: a freed range must be handed out
//! again, or a process that maps and unmaps a large buffer per frame (the
//! desktop's per-clip mask) exhausts the region with almost nothing resident.

use super::*;

/// A non-`MAP_FIXED` `mmap` of `len` bytes.
fn mmap_any(len: u64) -> u64 {
    process::linux::dispatch_args5_for_test(9, 0, len, PROT_RW, MAP_PRIVATE | MAP_ANONYMOUS, 0)
}

/// `munmap` makes the hole available to the next `mmap`, first-fit.
pub fn mmap_reuses_freed_range() -> Result<(), String> {
    fresh()?;
    let first = mmap_any(4 * PAGE);
    check!(first != 0 && (first as i64) > 0, "mmap returned {first:#x}");
    let second = mmap_any(4 * PAGE);
    check!(second != first, "two live mappings overlap at {first:#x}");
    check!(munmap(first, 4 * PAGE) == 0, "munmap failed");
    let third = mmap_any(4 * PAGE);
    check!(
        third == first,
        "freed range not reused: {third:#x} != {first:#x}"
    );
    check!(munmap(second, 4 * PAGE) == 0, "munmap second failed");
    check!(munmap(third, 4 * PAGE) == 0, "munmap third failed");
    Ok(())
}

/// Soak: map/unmap cycles totalling several times the whole mmap region must
/// keep succeeding (each cycle is one 345 KiB clip mask in the real failure).
pub fn mmap_munmap_soak_does_not_exhaust_region() -> Result<(), String> {
    fresh()?;
    let len = 345_620u64;
    let region = process::linux::MMAP_LIMIT - process::linux::MMAP_BASE;
    let cycles = 4 * region / len;
    for round in 0..cycles {
        let addr = mmap_any(len);
        check!(
            addr != 0 && (addr as i64) > 0,
            "round {round}/{cycles}: mmap failed with {addr:#x}"
        );
        check!(munmap(addr, len) == 0, "round {round}: munmap failed");
    }
    Ok(())
}

/// A small `mmap` lands above a large live mapping without one search step per
/// `len` bytes (the search skips a VMA at its full extent).
pub fn mmap_skips_a_large_mapping_in_one_step() -> Result<(), String> {
    fresh()?;
    let big_len = 64 * 1024 * 1024u64;
    let big = mmap_any(big_len);
    check!(big != 0 && (big as i64) > 0, "big mmap returned {big:#x}");
    let small = mmap_any(PAGE);
    check!(
        small >= big + big_len || small + PAGE <= big,
        "small mapping {small:#x} overlaps the big one at {big:#x}"
    );
    check!(munmap(small, PAGE) == 0, "munmap small failed");
    check!(munmap(big, big_len) == 0, "munmap big failed");
    Ok(())
}
