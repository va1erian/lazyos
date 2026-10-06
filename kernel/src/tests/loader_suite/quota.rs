//! ELF segments charged to the running uid's `UserMemory` quota (issue
//! #265): the loader charges the image's page span before mapping a frame,
//! refuses an image the uid cannot afford, and freeing the address space
//! (exit, a failed load, the image `execve` replaced) refunds it.

use super::*;
use crate::process::loader::{span_bytes_of, QUOTA_EXCEEDED};
use crate::quota::{self, Resource};

/// A regular uid for these tests.
const UID: u32 = 48_401;

/// Text and data sharing a boundary page, with a `.bss` tail: the page span
/// is `0x40_0000..0x40_5000`, five pages.
fn two_segments() -> Vec<u8> {
    let phdrs = [
        Ph::new(0x40_0000, 0x1800, 0x1800, PF_R | PF_X),
        Ph {
            offset: PAYLOAD_OFF + 0x1800,
            ..Ph::new(0x40_1800, 0x100, 0x2900, PF_R | PF_W)
        },
    ];
    build_elf(0x40_0010, &phdrs)
}

const SPAN: u64 = 0x5000;

fn used() -> u64 {
    quota::usage(UID, Resource::UserMemory)
}

/// The span arithmetic counts a shared boundary page once and never counts
/// a gap.
pub fn loader_span_counts_shared_pages_once() -> Result<(), String> {
    let cases: [(&[(u64, u64)], u64); 4] = [
        (&[(0x40_0000, 0x40_2000), (0x40_1000, 0x40_5000)], 0x5000),
        (&[(0x40_0000, 0x40_1000), (0x50_0000, 0x50_3000)], 0x4000),
        (&[(0, 0x1000)], 0x1000),
        (&[], 0),
    ];
    for (spans, expected) in cases {
        let got = span_bytes_of(spans);
        check!(
            got == expected,
            "{spans:x?}: {got:#x}, expected {expected:#x}"
        );
    }
    Ok(())
}

/// A load charges exactly the image's page span to the uid against the new
/// table, and freeing the table gives it back.
pub fn loader_charges_segments_to_the_uid() -> Result<(), String> {
    quota::reset();
    let elf = two_segments();
    let during = with_table(|table| {
        load_segments(table, &elf, RESERVED, UID)?;
        Ok(used())
    })?;
    check!(
        during == SPAN,
        "the load charged {during:#x}, expected {SPAN:#x}"
    );
    check!(used() == 0, "freeing the table left {:#x} charged", used());
    check!(
        quota::stats(UID).over_releases == 0,
        "the refund released more than was charged"
    );
    Ok(())
}

/// An image past the uid's quota is refused as out of memory before any
/// frame is mapped, and nothing stays charged.
pub fn loader_refuses_an_image_over_quota() -> Result<(), String> {
    quota::reset();
    quota::set_limit(UID, Resource::UserMemory, SPAN - 0x1000);
    let elf = two_segments();
    with_table(|table| {
        let before = mem::frame_stats().free;
        let result = load_segments(table, &elf, RESERVED, UID);
        check!(
            result.err() == Some(QUOTA_EXCEEDED),
            "an unaffordable image loaded: {result:?}"
        );
        check!(
            crate::process::loader::is_out_of_memory(QUOTA_EXCEEDED),
            "the quota refusal is not ENOMEM"
        );
        check!(
            mem::frame_stats().free == before,
            "the refused load mapped frames"
        );
        Ok(())
    })?;
    check!(used() == 0, "a refused load left {:#x} charged", used());
    quota::set_limit(UID, Resource::UserMemory, SPAN);
    with_table(|table| {
        load_segments(table, &elf, RESERVED, UID)
            .map(|_| ())
            .map_err(String::from)
    })?;
    check!(used() == 0, "an exact fit left {:#x} charged", used());
    quota::reset();
    Ok(())
}

/// Hundreds of loads, refused and accepted alternately, against a quota with
/// room for two images at once: the charge never drifts and no frame leaks.
pub fn loader_quota_soak() -> Result<(), String> {
    const ROUNDS: usize = 400;
    quota::reset();
    quota::set_limit(UID, Resource::UserMemory, 2 * SPAN);
    let elf = two_segments();
    for round in 0..ROUNDS {
        let outcome = with_table(|first| {
            load_segments(first, &elf, RESERVED, UID)?;
            with_table(|second| {
                load_segments(second, &elf, RESERVED, UID)?;
                with_table(|third| match load_segments(third, &elf, RESERVED, UID) {
                    Err(QUOTA_EXCEEDED) => Ok(()),
                    other => Err(format!("a third image got {other:?}")),
                })
            })
        });
        if let Err(error) = outcome {
            quota::reset();
            return Err(format!("round {round}: {error}"));
        }
        if used() != 0 {
            quota::reset();
            return Err(format!("round {round}: {:#x} left charged", used()));
        }
    }
    let stats = quota::stats(UID);
    quota::reset();
    check!(
        stats.peak[Resource::UserMemory.index()] == 2 * SPAN && stats.over_releases == 0,
        "after the soak: {stats:?}"
    );
    Ok(())
}
