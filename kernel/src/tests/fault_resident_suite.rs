//! Soak for the resident-only user reads the fault report uses
//! (`user_ptr::try_read_resident`, `try_resident_bytes`; the correctness
//! test is `fault_report_code_never_faults`): thousands of mixed reads, none
//! of which may allocate a frame or materialize a page.

use super::*;
use crate::mem::vma::{Kind, Prot};

const PAGE: u64 = 4096;

/// One xorshift step.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn soak_reads() -> Result<(), String> {
    let table = mem::kernel_table();
    // Two pages of a mapping; only the first is ever made present.
    let base = crate::process::linux::MMAP_BASE + 0x4300_0000;
    mem::vma::insert(
        table,
        base,
        base + 2 * PAGE,
        Prot::READ | Prot::WRITE,
        Kind::Anon,
    );
    let outcome = (|| {
        check!(
            crate::user_ptr::try_read::<u8>(base).is_ok(),
            "the first page did not materialize"
        );
        let free = mem::frame_stats().free;
        let mut rng = 0x7e51_d3a7_0000_0001u64;
        let (mut hits, mut misses) = (0u32, 0u32);
        for round in 0..10_000u32 {
            let offset = next(&mut rng) % (2 * PAGE);
            let len = 1 + (next(&mut rng) % 64) as usize;
            let at = base + offset;
            // The range is readable exactly when it ends inside the first page.
            let readable = offset + len as u64 <= PAGE;
            let got = match round % 3 {
                0 => crate::user_ptr::try_resident_bytes(at, len).map(|bytes| bytes.len()),
                1 => crate::user_ptr::try_read_resident::<u64>(at).map(|_| 8),
                _ => crate::user_ptr::try_resident_bytes(0x7000_dead_0000 + offset, len)
                    .map(|bytes| bytes.len()),
            };
            let want = match round % 3 {
                0 => readable,
                1 => offset + 8 <= PAGE,
                _ => false,
            };
            check!(
                got.is_ok() == want,
                "round {round}: kind {} at {at:#x} len {len}: {got:?}, want ok={want}",
                round % 3
            );
            if want {
                hits += 1;
            } else {
                misses += 1;
            }
        }
        check!(
            hits > 1000 && misses > 1000,
            "{hits} readable, {misses} not"
        );
        check!(
            mem::frame_stats().free == free,
            "the reads moved the free frame count from {free} to {}",
            mem::frame_stats().free
        );
        check!(
            raw_entry(table, base + PAGE).is_none(),
            "a read materialized the second page"
        );
        Ok(())
    })();
    mem::unmap_range(table, base, base + 2 * PAGE);
    mem::vma::remove(table, base, base + 2 * PAGE);
    outcome
}

/// Soak: 10,000 mixed resident reads (inside the present page, across into
/// the absent one, past the mapping, at a wild address) answer exactly as the
/// page table says and leave the frame count and the page table alone.
pub fn report_resident_reads_soak() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let outcome = soak_reads();
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    outcome
}

pub(super) const CASES: &[(&str, Test)] = &[(
    "fault_report_resident_reads_soak",
    report_resident_reads_soak,
)];
