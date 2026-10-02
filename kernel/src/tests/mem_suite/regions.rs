//! The usable-RAM map (`mem::regions`, H1 of `docs/real-pc-boot-plan.md`):
//! synthetic firmware maps shaped like a 32 GiB UEFI desktop, hostile maps,
//! and a soak over many random ones.

use super::*;
use crate::mem::regions::{UsableMap, PHYS_LIMIT};
use crate::mem::MAX_REGIONS;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// A map shaped like a real UEFI desktop with 32 GiB: low conventional RAM
/// cut into many touching descriptors (boot-services code/data, loader data,
/// conventional), reserved/ACPI holes below 4 GiB, the PCI hole, and RAM
/// remapped above 4 GiB as more touching descriptors. Returns the usable
/// regions in firmware order and the bytes above 1 MiB they cover.
fn uefi_32g_map() -> (Vec<(u64, u64)>, u64) {
    let mut usable = Vec::new();
    // Below 1 MiB: clamped away by the allocator, but present in the map.
    usable.push((0x1000, 0x9F000));
    // 1 MiB .. ~2 GiB as 40 touching descriptors of uneven size.
    let mut at = MIB;
    for i in 0..40u64 {
        let len = (1 + i % 7) * 13 * MIB;
        usable.push((at, at + len));
        at += len;
    }
    let low_run_end = at;
    // An ACPI/reserved hole, then 10 more touching descriptors to ~3 GiB.
    at += 64 * MIB;
    let second_start = at;
    for i in 0..10u64 {
        let len = (3 + i) * 8 * MIB;
        usable.push((at, at + len));
        at += len;
    }
    let second_end = at;
    // Above 4 GiB: 32 GiB minus what sits below the PCI hole, in 15 pieces.
    let below = (low_run_end - MIB) + (second_end - second_start);
    let high_total = 32 * GIB - below;
    let piece = (high_total / 15) & !0xFFF;
    let mut high = 4 * GIB;
    for i in 0..15u64 {
        let end = if i == 14 {
            4 * GIB + high_total
        } else {
            high + piece
        };
        usable.push((high, end));
        high = end;
    }
    let total = below + high_total;
    (usable, total)
}

fn collect(regions: &[(u64, u64)]) -> UsableMap {
    UsableMap::collect(regions.iter().copied())
}

/// Sorted, page-aligned, disjoint and non-touching: the invariant every
/// consumer of the map (untouched cursors, `contains`, the DMA pool) relies on.
fn well_formed(map: &UsableMap) -> Result<(), String> {
    check!(map.count <= MAX_REGIONS, "count {} over the cap", map.count);
    for i in 0..map.count {
        let (start, end) = (map.starts[i], map.ends[i]);
        check!(start < end, "range {i} empty: {start:#x}..{end:#x}");
        check!(
            start & 0xFFF == 0 && end & 0xFFF == 0,
            "range {i} unaligned"
        );
        check!(start >= 0x10_0000, "range {i} below 1 MiB");
        check!(end <= PHYS_LIMIT, "range {i} above the physical limit");
        if i > 0 {
            check!(
                map.ends[i - 1] < start,
                "ranges {} and {i} touch or overlap",
                i - 1
            );
        }
    }
    Ok(())
}

pub fn regions_uefi_32g_keeps_all_ram() -> Result<(), String> {
    let (usable, total) = uefi_32g_map();
    check!(
        usable.len() >= 60,
        "map has only {} usable regions",
        usable.len()
    );
    let map = collect(&usable);
    well_formed(&map)?;
    check!(
        map.dropped_ranges == 0,
        "dropped {} ranges",
        map.dropped_ranges
    );
    check!(
        map.count == 3,
        "{} ranges, want 3 (two low runs, one high)",
        map.count
    );
    check!(
        map.total_bytes() == total,
        "kept {:#x} of {total:#x}",
        map.total_bytes()
    );
    check!(
        map.highest() == 4 * GIB + (map.ends[2] - map.starts[2]),
        "highest wrong"
    );
    // The refcount table (one u32 per frame up to ~33 GiB, about 33 MiB) fits
    // in the first, large low range.
    let mut placed = map;
    let (phys, frames) = placed.place_refcounts().ok_or("table not placed")?;
    check!(phys == map.starts[0], "table at {phys:#x}");
    check!(
        frames as u64 * 4096 >= map.highest() / 1024,
        "table too small"
    );
    Ok(())
}

pub fn regions_unsorted_overlapping_coalesce() -> Result<(), String> {
    // Firmware order is not promised, and a buggy map can overlap itself.
    let map = collect(&[
        (8 * MIB, 12 * MIB),
        (2 * MIB, 4 * MIB),
        (3 * MIB, 9 * MIB),     // overlaps both neighbours
        (20 * MIB, 20 * MIB),   // empty
        (30 * MIB, 25 * MIB),   // inverted
        (0x10_0800, 0x10_1800), // less than one whole frame once aligned
        (40 * MIB + 1, 41 * MIB + 0x1FFF),
    ]);
    well_formed(&map)?;
    check!(map.count == 2, "{} ranges", map.count);
    check!(
        (map.starts[0], map.ends[0]) == (2 * MIB, 12 * MIB),
        "first range wrong"
    );
    check!(
        (map.starts[1], map.ends[1]) == (40 * MIB + 0x1000, 41 * MIB + 0x1000),
        "second range not trimmed to frames"
    );
    check!(map.seen == 7, "seen {}", map.seen);
    Ok(())
}

pub fn regions_overflow_keeps_largest_and_counts() -> Result<(), String> {
    // 200 disjoint ranges with gaps: more than the table holds. The biggest
    // ranges are kept, the rest counted, and no byte is lost unaccounted.
    let mut usable = Vec::new();
    let mut input = 0u64;
    for i in 0..200u64 {
        let start = 2 * MIB + i * 4 * MIB;
        let len = ((i * 37) % 13 + 1) * 0x1000;
        usable.push((start, start + len));
        input += len;
    }
    let map = collect(&usable);
    well_formed(&map)?;
    check!(map.count == MAX_REGIONS, "count {}", map.count);
    check!(
        map.dropped_ranges == 200 - MAX_REGIONS,
        "dropped {}",
        map.dropped_ranges
    );
    check!(
        map.total_bytes() + map.dropped_bytes == input,
        "kept {} + dropped {} != {input}",
        map.total_bytes(),
        map.dropped_bytes
    );
    let smallest_kept = (0..map.count)
        .map(|i| map.ends[i] - map.starts[i])
        .min()
        .unwrap_or(0);
    // Every dropped range was no larger than any kept one.
    let dropped_max = usable
        .iter()
        .filter(|(s, _)| !(0..map.count).any(|i| map.starts[i] == *s))
        .map(|(s, e)| e - s)
        .max()
        .unwrap_or(0);
    check!(
        dropped_max <= smallest_kept,
        "dropped {dropped_max} > kept {smallest_kept}"
    );
    Ok(())
}

pub fn regions_absurd_range_dropped_for_table() -> Result<(), String> {
    // A range near the 52-bit limit would need a multi-terabyte refcount
    // table: it is dropped (counted) and the real RAM still boots.
    let mut map = collect(&[
        (MIB, 256 * MIB),
        (PHYS_LIMIT - 8 * MIB, PHYS_LIMIT + 8 * MIB),
        (u64::MAX - 0x2000, u64::MAX),
    ]);
    well_formed(&map)?;
    let (phys, _) = map.place_refcounts().ok_or("not placed")?;
    check!(phys == MIB, "table at {phys:#x}");
    check!(
        map.count == 1 && map.dropped_ranges == 1,
        "count {} dropped {}",
        map.count,
        map.dropped_ranges
    );
    check!(
        map.dropped_bytes == 8 * MIB,
        "dropped {:#x}",
        map.dropped_bytes
    );
    // Nothing usable at all: placement reports it instead of looping.
    let mut empty = collect(&[(0, 0x9F000)]);
    check!(
        empty.place_refcounts().is_none(),
        "empty map placed a table"
    );
    Ok(())
}

pub fn regions_live_allocator_matches_boot_map() -> Result<(), String> {
    // The booted allocator's ranges obey the same invariant (QEMU's map).
    let stats = mem::frame_stats();
    check!(stats.total > 0, "no frames");
    let ranges = mem::usable_ranges();
    check!(!ranges.is_empty(), "no ranges recorded");
    for pair in ranges.windows(2) {
        check!(pair[0].1 < pair[1].0, "live ranges touch: {:x?}", pair);
    }
    Ok(())
}

/// xorshift64*: deterministic, no dependency.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

pub fn soak_regions_random_maps() -> Result<(), String> {
    // 500 random maps of 1..300 frame-aligned disjoint regions, some
    // touching, shuffled: the result is always well formed, every byte is
    // either kept or counted as dropped, and the table always places.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for round in 0..500 {
        let n = (next(&mut state) % 300 + 1) as usize;
        let mut regions = Vec::with_capacity(n);
        let mut at = MIB;
        let mut input = 0u64;
        for _ in 0..n {
            // Gap of zero (touching) about a third of the time.
            let gap = match next(&mut state) % 3 {
                0 => 0,
                _ => (next(&mut state) % 64 + 1) * 0x1000,
            };
            let len = (next(&mut state) % 4096 + 1) * 0x1000;
            at += gap;
            regions.push((at, at + len));
            input += len;
            at += len;
        }
        for i in (1..regions.len()).rev() {
            let j = (next(&mut state) % (i as u64 + 1)) as usize;
            regions.swap(i, j);
        }
        let mut map = collect(&regions);
        well_formed(&map).map_err(|e| format!("round {round}: {e}"))?;
        check!(
            map.total_bytes() + map.dropped_bytes == input,
            "round {round}: kept {} + dropped {} != {input}",
            map.total_bytes(),
            map.dropped_bytes
        );
        check!(map.seen == n, "round {round}: seen {}", map.seen);
        if map.dropped_ranges == 0 {
            // Without drops the count is exactly the number of touching runs.
            let mut sorted = regions.clone();
            sorted.sort_unstable();
            let runs = 1 + sorted.windows(2).filter(|w| w[0].1 != w[1].0).count();
            check!(
                map.count == runs,
                "round {round}: {} ranges, {runs} runs",
                map.count
            );
        }
        let before = map.total_bytes();
        if let Some((phys, frames)) = map.place_refcounts() {
            let fits = (0..map.count)
                .any(|i| map.starts[i] == phys && phys + frames as u64 * 0x1000 <= map.ends[i]);
            check!(fits, "round {round}: table outside its range");
        } else {
            check!(
                map.count == 0,
                "round {round}: table not placed with ranges left"
            );
        }
        check!(
            map.total_bytes() <= before,
            "round {round}: placement grew the map"
        );
    }
    Ok(())
}
