//! The `LAZYOS_SMP=1` cases of the idle suite: halts that drop the real
//! entry lock, and the gates that take it back.

use super::super::smp_tlb_suite::online_aps;
use super::*;
use crate::arch::{percpu, smp};
use crate::sync::entry;
use alloc::vec::Vec;
use core::arch::x86_64::_rdtsc;
use core::sync::atomic::AtomicU32;

/// Halts the boot CPU makes while the APs hammer the lock (`--soak`:
/// the long run).
const HALTS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    20_000
} else {
    300
};

static GOT_AT: AtomicU64 = AtomicU64::new(0);
static COUNTER: AtomicU64 = AtomicU64::new(0);
static INSIDE: AtomicU32 = AtomicU32::new(0);
static OVERLAPS: AtomicU64 = AtomicU64::new(0);
/// Set when the APs of the contention case should stop.
static STOP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn rdtsc() -> u64 {
    // SAFETY: reads the TSC; no memory or privilege effects.
    unsafe { _rdtsc() }
}

fn aps() -> Vec<usize> {
    smp::bring_up_for_test(percpu::MAX_CPUS);
    online_aps()
}

/// Wait (half a calibrated second) until `tickets` are outstanding.
fn wait_until_queued(tickets: u32) -> Result<(), String> {
    let bound = crate::arch::clock::cycles_per_tick() * 50;
    let start = rdtsc();
    while entry::outstanding() < tickets {
        if rdtsc().wrapping_sub(start) > bound {
            return Err(format!("{} tickets outstanding", entry::outstanding()));
        }
        smp::serve_pending();
        core::hint::spin_loop();
    }
    Ok(())
}

/// An AP's turn: take the lock, note when, drop it.
fn job_note_turn(_: u64) -> u64 {
    entry::acquire_for_test();
    GOT_AT.store(rdtsc(), Ordering::Release);
    entry::release_for_test();
    1
}

/// An AP queues for the lock the boot CPU holds; the boot CPU halts
/// once. The AP gets the lock inside that halt (between its start and
/// its end), so the halt dropped the hold; the boot CPU holds again
/// after, with its halt mark cleared.
pub(super) fn halt_drops_the_hold() -> Result<(), String> {
    let Some(&ap) = aps().first() else {
        return Ok(());
    };
    fresh();
    GOT_AT.store(0, Ordering::Release);
    let ticket = smp::start_on(ap, job_note_turn, 0).ok_or("AP offline")?;
    let queued = wait_until_queued(2);
    let before = rdtsc();
    // Ended by the next tick; the AP's turn falls inside it.
    entry::halt();
    let after = rdtsc();
    let held = entry::held_here();
    let marked = percpu::current().entry_halt.load(Ordering::Relaxed);
    if queued.is_err() {
        // Let the AP finish before failing, holding nothing it waits on.
        entry::release_for_test();
        smp::finish_on(ap, ticket, 10);
        entry::acquire_for_test();
    }
    queued?;
    let done = smp::finish_on(ap, ticket, 10);
    check!(held, "the halt returned without the hold");
    check!(!marked, "the halt left its CPU marked");
    check!(done == Some(1), "the AP never got the lock: {done:?}");
    let got = GOT_AT.load(Ordering::Acquire);
    check!(
        got > before && got < after,
        "the AP got the lock at {got}, outside the halt [{before}, {after}]"
    );
    Ok(())
}

/// A ring-0 entry into a marked halt takes the lock and clears the
/// mark (what an interrupt landing on `hlt` does).
pub(super) fn entry_into_halt() -> Result<(), String> {
    check!(entry::held_here(), "the suite runs without the boot's hold");
    let cpu = percpu::current();
    cpu.entry_halt.store(true, Ordering::Relaxed);
    entry::release_for_test();
    entry::enter(0, 0);
    let held = entry::held_here();
    let marked = cpu.entry_halt.swap(false, Ordering::Relaxed);
    if !held {
        entry::acquire_for_test();
    }
    check!(held, "a ring-0 entry into a halt did not take the lock");
    check!(!marked, "the entry left the halt mark set");
    entry::leave(0);
    check!(entry::held_here(), "a ring-0 resume dropped the hold");
    Ok(())
}

/// A ring-0 fault into a marked halt (`entry::fault_gate`) takes the
/// lock like any other gate, runs its handler held, and clears the mark.
pub(super) fn fault_in_halt() -> Result<(), String> {
    check!(entry::held_here(), "the suite runs without the boot's hold");
    let cpu = percpu::current();
    cpu.entry_halt.store(true, Ordering::Relaxed);
    entry::release_for_test();
    let held_inside = entry::fault_gate(0, 0, entry::held_here, |_| 0);
    let held = entry::held_here();
    let marked = cpu.entry_halt.swap(false, Ordering::Relaxed);
    if !held {
        entry::acquire_for_test();
    }
    check!(held_inside, "the fault handler ran without the lock");
    check!(held, "the fault gate left without the hold");
    check!(!marked, "the fault gate left the halt mark set");
    Ok(())
}

fn section() {
    if INSIDE.fetch_add(1, Ordering::AcqRel) != 0 {
        OVERLAPS.fetch_add(1, Ordering::Relaxed);
    }
    let value = COUNTER.load(Ordering::Relaxed);
    for _ in 0..32 {
        core::hint::spin_loop();
    }
    COUNTER.store(value + 1, Ordering::Relaxed);
    INSIDE.fetch_sub(1, Ordering::AcqRel);
}

/// Sections under the lock until [`STOP`]; how many.
fn job_sections_until_stopped(_: u64) -> u64 {
    let mut rounds = 0;
    while !STOP.load(Ordering::Acquire) {
        entry::acquire_for_test();
        section();
        entry::release_for_test();
        rounds += 1;
    }
    rounds
}

/// Every AP takes and drops the lock around a section until told to
/// stop, while the boot CPU, holding it except inside its halts, makes
/// [`HALTS`] halts with a section before each: naps (a halt in this
/// task, ended by the tick) and 100 us sleeps (a halt in the idle
/// context, ended by the deadline timer) in turn. The APs only get in
/// during those halts, and each interrupt that ends one takes the lock
/// back against them: no update lost, no overlap, every AP got in.
pub(super) fn halts_under_contention() -> Result<(), String> {
    let aps = aps();
    if aps.is_empty() {
        return Ok(());
    }
    fresh();
    COUNTER.store(0, Ordering::Relaxed);
    OVERLAPS.store(0, Ordering::Relaxed);
    STOP.store(false, Ordering::Release);
    let mut started = Vec::new();
    for &ap in &aps {
        if let Some(ticket) = smp::start_on(ap, job_sections_until_stopped, 0) {
            started.push((ap, ticket));
        }
    }
    let (waits_before, _) = entry::contended();
    let loops = idle::probe::loops();
    for halt in 0..HALTS {
        section();
        if halt % 2 == 0 {
            task::nap();
        } else {
            super::nap_ns(100_000);
        }
    }
    let idled = idle::probe::loops() - loops;
    STOP.store(true, Ordering::Release);
    // The APs finish their last section once they get the lock.
    entry::release_for_test();
    let finished: Vec<_> = started
        .iter()
        .map(|&(ap, ticket)| smp::finish_on(ap, ticket, 10))
        .collect();
    entry::acquire_for_test();
    let ap_sections: u64 = finished.iter().flatten().sum();
    let expected = ap_sections + HALTS;
    let counted = COUNTER.load(Ordering::Relaxed);
    let (waits, _) = entry::contended();
    crate::trace::test!(
        "smp_idle_halts_under_contention:INFO:aps={} halts={HALTS} idle_passes={idled} sections={expected} waits={}",
        started.len(),
        waits - waits_before
    );
    check!(
        started.len() == aps.len(),
        "only {} APs started",
        started.len()
    );
    check!(
        finished
            .iter()
            .all(|done| done.is_some_and(|rounds| rounds > 0)),
        "an AP never got in or never stopped: {finished:?}"
    );
    check!(OVERLAPS.load(Ordering::Relaxed) == 0, "sections overlapped");
    check!(
        counted == expected,
        "{counted} of {expected} updates survived"
    );
    check!(
        idled >= HALTS / 4,
        "only {idled} idle passes in {} sleeps",
        HALTS / 2
    );
    Ok(())
}
