//! The virtio-blk request planner (`block/virtio/plan.rs`): every piece stays
//! inside one page, a request ends on a sector, the pieces of successive
//! requests cover the transfer exactly once and in order.

use super::*;
use crate::block::virtio::plan::{plan, Cursor, PlanError, MAX_PIECES};
use crate::block::SECTOR_SIZE;

const PAGE: u64 = 4096;

/// A translation that offsets every address, so a mix-up of virtual and
/// physical addresses shows.
const PHYS_OFFSET: u64 = 0x1_0000_0000;

fn identity(virt: u64) -> Option<u64> {
    Some(virt + PHYS_OFFSET)
}

/// Plan every request of a transfer over `segments`, checking each against
/// the rules, and the whole against the segments.
fn plan_all(segments: &[(u64, usize)], max_bytes: usize) -> Result<usize, String> {
    let total: usize = segments.iter().map(|&(_, len)| len).sum();
    let mut cursor = Cursor::default();
    let mut done = 0usize;
    let mut requests = 0usize;
    // The stream of bytes as (virtual address) the pieces must reproduce.
    let mut expect = segments
        .iter()
        .flat_map(|&(base, len)| (0..len as u64).step_by(1).map(move |i| base + i));
    while done < total {
        let request = plan(segments, cursor, max_bytes, identity)
            .map_err(|e| format!("request {requests} at {done}: {e:?}"))?;
        check!(
            request.bytes % SECTOR_SIZE == 0 && request.bytes > 0,
            "request {requests}: {} bytes",
            request.bytes
        );
        check!(request.bytes <= max_bytes, "request over {max_bytes}");
        check!(request.count <= MAX_PIECES, "{} pieces", request.count);
        let mut sum = 0usize;
        for &(phys, len) in &request.pieces[..request.count] {
            let virt = phys - PHYS_OFFSET;
            check!(len > 0, "an empty piece");
            check!(
                virt / PAGE == (virt + u64::from(len) - 1) / PAGE,
                "piece {virt:#x}+{len} crosses a page"
            );
            // Spot-check the stream at both ends of the piece.
            check!(
                expect.next() == Some(virt),
                "piece {virt:#x} is not where the stream is"
            );
            if len > 1 {
                let last = expect.nth(len as usize - 2);
                check!(
                    last == Some(virt + u64::from(len) - 1),
                    "piece {virt:#x}+{len} skips bytes"
                );
            }
            sum += len as usize;
        }
        check!(
            sum == request.bytes,
            "pieces sum to {sum}, not {}",
            request.bytes
        );
        cursor.advance(segments, request.bytes);
        done += request.bytes;
        requests += 1;
    }
    check!(expect.next().is_none(), "bytes left over");
    Ok(requests)
}

/// Fixed shapes: page-aligned pages (one request per 256 KiB), a buffer
/// starting mid-page, sector-sized segments straddling pages, and a segment
/// list whose pieces outnumber one request's descriptors.
pub fn pieces_follow_the_rules() -> Result<(), String> {
    let aligned: Vec<(u64, usize)> = (0..128u64)
        .map(|i| (0x10_0000 + i * PAGE * 3, 4096))
        .collect();
    check!(
        plan_all(&aligned, 256 * 1024)? == 2,
        "aligned pages: not two requests"
    );
    let unaligned = plan_all(&[(0x20_0123, 1 << 20)], 256 * 1024)?;
    check!(
        (4..=5).contains(&unaligned),
        "an unaligned MiB took {unaligned}"
    );
    let straddling: Vec<(u64, usize)> = (0..200u64).map(|i| (0x30_0F00 + i * PAGE, 512)).collect();
    plan_all(&straddling, 256 * 1024)?;
    let tiny_pieces: Vec<(u64, usize)> = (0..300u64)
        .map(|i| (0x40_0000 + i * PAGE * 2 + 3584, 1024))
        .collect();
    let requests = plan_all(&tiny_pieces, 256 * 1024)?;
    check!(
        requests >= 300 * 2 / MAX_PIECES,
        "{requests} requests for 600 pieces"
    );
    Ok(())
}

/// An unmapped page fails the plan; pieces that hold less than a sector are
/// refused rather than sent short.
pub fn unmapped_and_tiny() -> Result<(), String> {
    let segments = [(0x50_0000u64, 8192usize)];
    let hole = |virt: u64| (virt < 0x50_1000).then_some(virt);
    // The first page translates, the second does not: the plan fails.
    check!(
        plan(&segments, Cursor::default(), 256 * 1024, hole).err() == Some(PlanError::Unmapped),
        "an unmapped page was planned"
    );
    let crumbs: Vec<(u64, usize)> = (0..MAX_PIECES as u64 + 1)
        .map(|i| (0x60_0000 + i * PAGE, 4))
        .collect();
    check!(
        plan(&crumbs, Cursor::default(), 256 * 1024, identity).err() == Some(PlanError::Tiny),
        "sub-sector pieces were planned"
    );
    Ok(())
}

/// Thousands of random segment lists (random alignment, lengths and request
/// sizes) all plan by the rules.
pub fn soak_random_segments() -> Result<(), String> {
    let mut rng = Rng(0x5EED_0F_B10C);
    for round in 0..3000 {
        let count = 1 + rng.below(40) as usize;
        let mut segments = Vec::new();
        let mut sectors = 0usize;
        for index in 0..count {
            let len = match rng.below(4) {
                0 => 512 * (1 + rng.below(4) as usize),
                1 => 4096,
                2 => 512 * (1 + rng.below(300) as usize),
                _ => 1 + rng.below(3000) as usize,
            };
            let base = 0x100_0000 + index as u64 * 0x10_0000 + rng.below(PAGE);
            segments.push((base, len));
            sectors += len;
        }
        // Make the total a whole number of sectors.
        let short = (SECTOR_SIZE - sectors % SECTOR_SIZE) % SECTOR_SIZE;
        if short > 0 {
            segments.push((0x900_0000 + rng.below(PAGE), short));
        }
        let max = SECTOR_SIZE * (1 + rng.below(512) as usize);
        match plan_all(&segments, max) {
            Ok(_) => {}
            // Sub-sector crumbs can legitimately make one request impossible.
            Err(error) if error.contains("Tiny") => {}
            Err(error) => return Err(format!("round {round}: {error}")),
        }
    }
    Ok(())
}
