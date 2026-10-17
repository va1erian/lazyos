//! The runnable-wait ledger (`perf/runwait.rs`): what a task waited between becoming runnable and
//! running, kept per slot. The ledger is plain arithmetic, so the rules and a long run of them are
//! checked here directly, not through the scheduler's hooks (those exist only with `LAZYOS_PERF=1`).

use super::*;
use crate::perf::runwait::Ledger;
use crate::task::MAX_TASKS;

/// The ledgers are statics: one is 28 KiB, too much for a test task's kernel stack.
static RULES: Ledger = Ledger::new();
static SOAK: Ledger = Ledger::new();

pub(super) fn runwait_ledger_rules() -> Result<(), String> {
    let ledger = &RULES;
    ledger.run(3, 500);
    check!(
        ledger.totals(3) == (0, 0, 0),
        "a run with no wait counts nothing"
    );
    ledger.ready(3, 1_000);
    ledger.run(3, 1_400);
    check!(
        ledger.totals(3) == (1, 400, 400),
        "one wait of 400: {:?}",
        ledger.totals(3)
    );
    ledger.run(3, 9_000);
    check!(
        ledger.totals(3).0 == 1,
        "a task that is running is not waiting again"
    );
    ledger.ready(3, 2_000);
    ledger.run(3, 2_100);
    check!(
        ledger.totals(3) == (2, 500, 400),
        "totals add, longest stays: {:?}",
        ledger.totals(3)
    );
    // Ready again before it ran: the later time is when it waited from.
    ledger.ready(4, 100);
    ledger.ready(4, 700);
    ledger.run(4, 1_000);
    check!(ledger.totals(4) == (1, 300, 300), "{:?}", ledger.totals(4));
    // Slots do not share counters, and a time of 0 still marks a wait.
    ledger.ready(5, 0);
    ledger.run(5, 50);
    check!(ledger.totals(5) == (1, 49, 49), "{:?}", ledger.totals(5));
    check!(ledger.totals(6) == (0, 0, 0), "an untouched slot");
    // Past the table: ignored, never a panic.
    ledger.ready(MAX_TASKS, 1);
    ledger.run(MAX_TASKS + 7, 2);
    check!(ledger.totals(MAX_TASKS) == (0, 0, 0), "past the table");
    Ok(())
}

pub(super) fn runwait_ledger_soak() -> Result<(), String> {
    let ledger = &SOAK;
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let (mut waits, mut total, mut longest) = ([0u32; 8], [0u64; 8], [0u64; 8]);
    let mut since = [0u64; 8];
    let mut now = 1u64;
    for _ in 0..2_000_000u32 {
        now += 1 + next() % 90;
        let slot = (next() % 8) as usize;
        if since[slot] == 0 {
            ledger.ready(slot, now);
            since[slot] = now;
        } else if next() % 3 != 0 {
            ledger.run(slot, now);
            let waited = now - since[slot];
            waits[slot] += 1;
            total[slot] += waited;
            longest[slot] = longest[slot].max(waited);
            since[slot] = 0;
        }
    }
    for slot in 0..8 {
        check!(
            ledger.totals(slot) == (waits[slot], total[slot], longest[slot]),
            "slot {slot}: {:?} against {:?}",
            ledger.totals(slot),
            (waits[slot], total[slot], longest[slot])
        );
    }
    Ok(())
}
