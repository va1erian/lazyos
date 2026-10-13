//! Hitting the per-uid user-memory quota (`limit.quota_user_memory`) through
//! `mmap` is a clean `ENOMEM` (issue #774): no panic, no stuck allocator, the
//! address space untouched by the refusal, and every byte charged comes back
//! on `munmap` and when the process exits.

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::quota::{self, Resource};

/// A regular uid with a quota of its own.
pub(super) const UID: u32 = 48_411;
pub(super) const MIB: u64 = 1 << 20;
const ENOMEM: u64 = (-12i64) as u64;

fn mmap_any(len: u64) -> u64 {
    process::linux::dispatch_args5_for_test(9, 0, len, PROT_RW, MAP_PRIVATE | MAP_ANONYMOUS, 0)
}

pub(super) fn used() -> u64 {
    quota::usage(UID, Resource::UserMemory)
}

/// Run the kernel task as [`UID`] with `limit` bytes of user memory; the
/// ledger starts empty and root comes back afterwards.
pub(super) fn with_quota(
    limit: u64,
    body: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    fresh()?;
    quota::reset();
    quota::set_limit(UID, Resource::UserMemory, limit);
    credentials::set(task::KERNEL_TASK, Cred::new(UID, UID, 0, 0, 0));
    let outcome = body();
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    outcome
}

/// Map `chunk` bytes until the kernel says no; the addresses, and what it
/// said.
fn map_until_refused(chunk: u64) -> Result<(Vec<u64>, u64), String> {
    let mut maps = Vec::new();
    loop {
        let addr = mmap_any(chunk);
        if addr as i64 <= 0 {
            return Ok((maps, addr));
        }
        maps.push(addr);
        check!(maps.len() <= 4096, "the quota never bit");
    }
}

/// Mappings stop exactly at the quota with `ENOMEM`, a refused call changes
/// nothing (no charge, no frames), room made by `munmap` is usable at once,
/// and unmapping everything returns the whole charge.
pub fn mmap_quota_exhaustion_is_enomem() -> Result<(), String> {
    with_quota(64 * MIB, || {
        let frames = mem::frame_stats().live();
        let (maps, refusal) = map_until_refused(4 * MIB)?;
        check!(refusal == ENOMEM, "the quota gave {refusal:#x}, not ENOMEM");
        check!(maps.len() == 16, "{} mappings before ENOMEM", maps.len());
        check!(used() == 64 * MIB, "{} bytes charged", used());
        // Demand-zero: reserving the quota took no frames.
        let taken = mem::frame_stats().live() - frames;
        check!(taken <= 8, "{taken} frames spent on untouched mappings");

        for len in [PAGE, 1 << 40, u64::MAX / 2, u64::MAX - PAGE] {
            let again = mmap_any(len);
            check!(
                again == ENOMEM,
                "mmap({len:#x}) at the quota gave {again:#x}"
            );
        }
        check!(
            used() == 64 * MIB,
            "a refusal changed the charge to {}",
            used()
        );

        check!(munmap(maps[3], 4 * MIB) == 0, "munmap failed");
        let reused = mmap_any(4 * MIB);
        check!(reused as i64 > 0, "no room after munmap: {reused:#x}");
        check!(mmap_any(PAGE) == ENOMEM, "the quota was exceeded again");
        check!(munmap(reused, 4 * MIB) == 0, "munmap of the reused range");

        for (index, &addr) in maps.iter().enumerate() {
            if index != 3 {
                check!(munmap(addr, 4 * MIB) == 0, "munmap {index} failed");
            }
        }
        check!(used() == 0, "{} bytes still charged", used());
        let stats = quota::stats(UID);
        check!(
            stats.over_releases == 0 && stats.denials >= 3,
            "ledger after the run: {stats:?}"
        );
        Ok(())
    })
}

/// Soak: 2,000 rounds of mapping to the quota and unmapping it all. Every
/// round reaches the same count with `ENOMEM`, the same addresses come back,
/// and nothing stays charged or resident.
pub fn mmap_quota_exhaustion_soak() -> Result<(), String> {
    const ROUNDS: usize = 2000;
    with_quota(8 * MIB, || {
        let mut first: Vec<u64> = Vec::new();
        let frames = mem::frame_stats().live();
        for round in 0..ROUNDS {
            let (maps, refusal) = map_until_refused(MIB)?;
            check!(
                refusal == ENOMEM && maps.len() == 8,
                "round {round}: {} mappings, then {refusal:#x}",
                maps.len()
            );
            if round == 0 {
                first = maps.clone();
            } else {
                check!(maps == first, "round {round}: the ranges were not reused");
            }
            for addr in maps {
                check!(munmap(addr, MIB) == 0, "round {round}: munmap failed");
            }
            check!(used() == 0, "round {round}: {} bytes charged", used());
        }
        let leaked = mem::frame_stats().live().saturating_sub(frames);
        check!(leaked <= 8, "{leaked} frames leaked over {ROUNDS} rounds");
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

/// A process that dies at its quota gives all of it back: the charge is
/// recorded per address space and refunded when the space is torn down, so a
/// child that never unmapped anything leaves the ledger empty once reaped.
pub fn mmap_quota_returns_when_the_process_exits() -> Result<(), String> {
    with_quota(64 * MIB, || {
        let child = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
        credentials::set(child, Cred::new(UID, UID, 0, 0, 0));
        let mut charged = 0;
        while quota::charge_for_slot(child, Resource::UserMemory, 16 * MIB).is_ok() {
            charged += 1;
            check!(charged <= 16, "the quota never bit");
        }
        check!(charged == 4, "{charged} chunks fit a 64 MiB quota");
        check!(used() == 64 * MIB, "{} bytes charged", used());
        check!(
            quota::charge_for_slot(child, Resource::UserMemory, PAGE).is_err(),
            "a page fitted past the quota"
        );

        task::harness::finish(child, 9);
        task::harness::switch_current(task::KERNEL_TASK);
        let (reaped, _) = task::reap_child().ok_or("the child was not reapable")?;
        check!(reaped == child, "reaped {reaped}, expected {child}");
        check!(used() == 0, "{} bytes charged after the exit", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        task::harness::reset();
        Ok(())
    })
}
