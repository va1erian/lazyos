//! Boot-phase timestamps (`boot_trace`): monotonic stamps, ring bookkeeping,
//! and a soak that wraps the ring many times.

use super::*;
use crate::boot_trace;

fn tsc_is_monotonic() -> Result<(), String> {
    let mut last = boot_trace::tsc();
    for _ in 0..10_000 {
        let now = boot_trace::tsc();
        check!(now >= last, "tsc went backwards: {last} -> {now}");
        last = now;
    }
    Ok(())
}

fn record_returns_sequence_numbers() -> Result<(), String> {
    let first = boot_trace::record(11);
    let second = boot_trace::record(22);
    check!(second == first + 1, "sequence {first} then {second}");
    check!(boot_trace::stamp(first) == Some(11), "stamp {first} lost");
    check!(boot_trace::stamp(second) == Some(22), "stamp {second} lost");
    check!(
        boot_trace::stamp(boot_trace::count()).is_none(),
        "a stamp not yet recorded must read back as None"
    );
    Ok(())
}

fn stale_stamps_fall_out_of_the_ring() -> Result<(), String> {
    let old = boot_trace::record(7);
    for i in 0..64u64 {
        boot_trace::record(1000 + i);
    }
    check!(
        boot_trace::stamp(old).is_none(),
        "a stamp older than the ring must not alias a newer slot"
    );
    let newest = boot_trace::count() - 1;
    check!(
        boot_trace::stamp(newest) == Some(1063),
        "newest stamp wrong"
    );
    Ok(())
}

fn mark_counts_and_stays_monotonic() -> Result<(), String> {
    let before = boot_trace::count();
    boot_phase!("test_a");
    boot_phase!("test_b_{}", 2);
    check!(boot_trace::count() == before + 2, "mark did not record");
    let a = boot_trace::stamp(before).ok_or("stamp a")?;
    let b = boot_trace::stamp(before + 1).ok_or("stamp b")?;
    check!(b >= a, "marks out of order: {a} then {b}");
    Ok(())
}

/// Soak: a million records wrap the 32-slot ring ~31k times; the newest
/// stamps must always read back exactly and the counter must not skip.
fn ring_soak() -> Result<(), String> {
    let start = boot_trace::count();
    for i in 0..1_000_000u64 {
        let index = boot_trace::record(i);
        check!(index == start + i as usize, "sequence skipped at {i}");
    }
    let newest = boot_trace::count() - 1;
    check!(
        boot_trace::stamp(newest) == Some(999_999),
        "newest stamp wrong after soak"
    );
    check!(
        boot_trace::stamp(newest - 31) == Some(999_968),
        "oldest in-ring stamp wrong after soak"
    );
    check!(
        boot_trace::stamp(newest - 32).is_none(),
        "stamp beyond the ring must be gone"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("boot_trace_tsc_monotonic", tsc_is_monotonic),
    (
        "boot_trace_record_sequence",
        record_returns_sequence_numbers,
    ),
    (
        "boot_trace_ring_evicts_stale",
        stale_stamps_fall_out_of_the_ring,
    ),
    ("boot_trace_mark_counts", mark_counts_and_stays_monotonic),
    ("boot_trace_ring_soak", ring_soak),
];
