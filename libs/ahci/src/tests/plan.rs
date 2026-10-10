//! PRDT planning: page crossings, merging, odd lengths, limits.

use std::vec::Vec;

use crate::cmd::{plan, Cursor, PlanError, MAX_PRD};

fn identity(virt: u64) -> Option<u64> {
    Some(virt)
}

/// Pages scattered so no two neighbours are adjacent.
fn scatter(virt: u64) -> Option<u64> {
    Some(((virt / 4096) * 3 % 1_000_003) * 8192 + virt % 4096)
}

#[test]
fn one_buffer_one_entry_when_contiguous() {
    let plan = plan(
        &[(0x10_0000, 8192)],
        Cursor::default(),
        1 << 20,
        512,
        true,
        &identity,
    )
    .unwrap();
    assert_eq!(plan.bytes, 8192);
    assert_eq!(plan.entries().len(), 1, "adjacent pages merge");
    assert_eq!(plan.entries()[0].bytes, 8192);
}

#[test]
fn scattered_pages_split_at_page_boundaries() {
    let plan = plan(
        &[(0x10_0200, 8192)],
        Cursor::default(),
        1 << 20,
        512,
        true,
        &scatter,
    )
    .unwrap();
    assert_eq!(plan.bytes, 8192);
    let sizes: Vec<u32> = plan.entries().iter().map(|prd| prd.bytes).collect();
    assert_eq!(sizes, [3584, 4096, 512]);
}

#[test]
fn limit_is_max_bytes() {
    let plan = plan(
        &[(0x10_0000, 1 << 20)],
        Cursor::default(),
        256 * 1024,
        512,
        true,
        &identity,
    )
    .unwrap();
    assert_eq!(plan.bytes, 256 * 1024);
}

#[test]
fn entry_limit_trims_to_whole_sectors() {
    // 64 entries of 4 KiB scattered pages, but the first begins 2 bytes
    // short of a page end so the count is not sector aligned at entry 64.
    let segments = [(0x10_0000 + 4096 - 2, 1 << 20)];
    let plan = plan(&segments, Cursor::default(), 1 << 20, 512, true, &scatter).unwrap();
    assert!(plan.entries().len() <= MAX_PRD);
    assert_eq!(plan.bytes % 512, 0);
    let total: u32 = plan.entries().iter().map(|prd| prd.bytes).sum();
    assert_eq!(total as usize, plan.bytes);
    assert!(plan
        .entries()
        .iter()
        .all(|prd| prd.bytes % 2 == 0 && prd.bytes > 0));
}

#[test]
fn odd_address_or_length_needs_the_bounce_page() {
    for segment in [(0x10_0001u64, 512usize), (0x10_0000, 511)] {
        let result = plan(&[segment], Cursor::default(), 1 << 20, 512, true, &identity);
        assert_eq!(result.unwrap_err(), PlanError::Misaligned, "{segment:?}");
    }
    // Two-byte alignment is enough.
    assert!(plan(
        &[(0x10_0002, 512)],
        Cursor::default(),
        1 << 20,
        512,
        true,
        &identity
    )
    .is_ok());
}

#[test]
fn many_tiny_segments_cannot_make_a_sector() {
    let segments: Vec<(u64, usize)> = (0..200).map(|i| (0x10_0000 + i * 0x2000, 2)).collect();
    let result = plan(&segments, Cursor::default(), 1 << 20, 512, true, &scatter);
    assert_eq!(result.unwrap_err(), PlanError::Misaligned);
}

#[test]
fn unmapped_page_is_reported() {
    let result = plan(
        &[(0x10_0000, 512)],
        Cursor::default(),
        1 << 20,
        512,
        true,
        &|_| None,
    );
    assert_eq!(result.unwrap_err(), PlanError::Unmapped);
}

#[test]
fn memory_above_4g_needs_64_bit_addressing() {
    let high = |virt: u64| Some((1u64 << 32) + virt);
    assert_eq!(
        plan(
            &[(0x1000, 512)],
            Cursor::default(),
            1 << 20,
            512,
            false,
            &high
        )
        .unwrap_err(),
        PlanError::Misaligned
    );
    assert!(plan(
        &[(0x1000, 512)],
        Cursor::default(),
        1 << 20,
        512,
        true,
        &high
    )
    .is_ok());
    // Straddling the 4 GiB line also counts.
    let edge = |virt: u64| Some((1u64 << 32) - 256 + virt % 4096);
    assert!(plan(
        &[(0x1000, 512)],
        Cursor::default(),
        1 << 20,
        512,
        false,
        &edge
    )
    .is_err());
}

#[test]
fn cursor_walks_segments() {
    let segments = [(0x10_0000u64, 1024usize), (0x20_0000, 2048)];
    let mut cursor = Cursor::default();
    cursor.advance(&segments, 1536);
    assert_eq!((cursor.segment, cursor.offset), (1, 512));
    let plan = plan(&segments, cursor, 1 << 20, 512, true, &identity).unwrap();
    assert_eq!(plan.bytes, 1536);
    assert_eq!(plan.entries()[0].addr, 0x20_0200);
}
