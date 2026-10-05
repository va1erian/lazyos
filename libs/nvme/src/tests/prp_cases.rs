//! PRP planner cases: page-aligned and unaligned buffers, merges, the entry
//! limit, and trimming to a block boundary.

use crate::prp::{self, Cursor, MAX_ENTRIES};
use crate::PAGE;

fn identity(virt: u64) -> Option<u64> {
    Some(virt)
}

/// Scatter every page so nothing is physically contiguous across pages.
fn scattered(virt: u64) -> Option<u64> {
    Some((virt / PAGE) * 7 * PAGE + 0x1_0000_0000 + virt % PAGE)
}

#[test]
fn one_page_two_pages_and_a_list() {
    let plan = prp::plan(
        &[(0x10_000, 4096)],
        Cursor::default(),
        65536,
        512,
        &scattered,
    )
    .unwrap();
    assert_eq!((plan.count, plan.bytes), (1, 4096));
    assert!(!plan.needs_list() && plan.prp2_direct() == 0);
    let plan = prp::plan(
        &[(0x10_000, 8192)],
        Cursor::default(),
        65536,
        512,
        &scattered,
    )
    .unwrap();
    assert_eq!(plan.count, 2);
    assert_eq!(plan.prp2_direct(), scattered(0x11_000).unwrap());
    let plan = prp::plan(
        &[(0x10_000, 12288)],
        Cursor::default(),
        65536,
        512,
        &scattered,
    )
    .unwrap();
    assert!(plan.needs_list());
    let mut list = [0u8; MAX_ENTRIES * 8];
    assert_eq!(plan.list(&mut list), 16);
    assert!(prp::valid(&plan));
}

#[test]
fn unaligned_start_spans_one_more_page() {
    let plan = prp::plan(
        &[(0x10_200, 65536)],
        Cursor::default(),
        65536,
        512,
        &scattered,
    )
    .unwrap();
    assert_eq!(plan.count, MAX_ENTRIES);
    assert_eq!(plan.bytes, 65536);
    assert!(prp::valid(&plan));
}

#[test]
fn adjacent_buffers_in_one_page_merge() {
    let segments = [
        (0x10_000, 1024),
        (0x10_400, 1024),
        (0x10_800, 2048),
        (0x11_000, 4096),
    ];
    let plan = prp::plan(&segments, Cursor::default(), 65536, 512, &identity).unwrap();
    assert_eq!((plan.count, plan.bytes), (2, 8192));
    assert!(prp::valid(&plan));
}

#[test]
fn a_break_mid_page_ends_the_command() {
    let segments = [(0x10_000, 1024), (0x20_000, 1024)];
    let plan = prp::plan(&segments, Cursor::default(), 65536, 512, &scattered).unwrap();
    assert_eq!(plan.bytes, 1024);
    let mut cursor = Cursor::default();
    cursor.advance(&segments, plan.bytes);
    assert_eq!(
        cursor,
        Cursor {
            segment: 1,
            offset: 0
        }
    );
    let next = prp::plan(&segments, cursor, 65536, 512, &scattered).unwrap();
    assert_eq!(next.bytes, 1024);
}

#[test]
fn the_byte_limit_splits_a_segment_on_a_block() {
    let segments = [(0x10_000, 20 * 1024)];
    let plan = prp::plan(&segments, Cursor::default(), 8192, 512, &scattered).unwrap();
    assert_eq!(plan.bytes, 8192);
    let mut cursor = Cursor::default();
    cursor.advance(&segments, plan.bytes);
    let next = prp::plan(&segments, cursor, 8192, 512, &scattered).unwrap();
    assert_eq!(next.prp1(), scattered(0x12_000).unwrap());
}

#[test]
fn trims_back_to_the_block() {
    // 1536 bytes then a break, with 1 KiB blocks: one block goes.
    let segments = [(0x10_800, 1536), (0x30_000, 1024)];
    let plan = prp::plan(&segments, Cursor::default(), 65536, 1024, &scattered).unwrap();
    assert_eq!(plan.bytes, 1024);
    assert!(prp::valid(&plan));
}

#[test]
fn empty_segments_are_skipped() {
    let segments = [(0x10_000, 0), (0x20_000, 512)];
    let mut cursor = Cursor::default();
    cursor.advance(&segments, 0);
    assert_eq!(cursor.segment, 1);
    let plan = prp::plan(&segments, cursor, 65536, 512, &identity).unwrap();
    assert_eq!(plan.prp1(), 0x20_000);
}
