//! Stress: many timers armed and disarmed, an interval timer across many
//! ticks, and fork/exit churn that must not leak timers or signal state.

use super::*;
use crate::task::itimer::{self, Kind, Notify};
use crate::task::signal::SIGALRM;

/// A tiny deterministic generator so a failing round reproduces.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

/// Thousands of arm / re-arm / disarm / create / delete operations across
/// several address spaces, the heap invariants checked along the way and the
/// table empty at the end.
pub fn arm_disarm() -> Result<(), String> {
    fresh()?;
    let mut rng = Lcg(0x5eed);
    let spaces = [
        0x10_000u64,
        0x20_000,
        0x30_000,
        0x40_000,
        0x50_000,
        0x60_000,
    ];
    let mut ids: [alloc::vec::Vec<i32>; 6] = Default::default();
    for round in 0..4000u32 {
        let which = (rng.next() % 6) as usize;
        let pml4 = spaces[which];
        let t = now();
        match rng.next() % 6 {
            0 => {
                let kind = [Kind::Real, Kind::Virtual, Kind::Prof][(rng.next() % 3) as usize];
                let value = rng.next() % 3 * NS / 2;
                itimer::set_interval(pml4, 1, kind, t, value, value / 2)
                    .map_err(|e| format!("round {round}: set_interval {e:?}"))?;
            }
            1 => {
                if ids[which].len() < 20 {
                    let id = itimer::create(pml4, 1, Notify::Process, SIGALRM, None, 1)
                        .map_err(|e| format!("round {round}: create {e:?}"))?;
                    ids[which].push(id);
                }
            }
            2 | 3 => {
                if let Some(&id) = ids[which].get((rng.next() % 20) as usize) {
                    let start = (rng.next() % 4 != 0).then(|| t + rng.next() % NS);
                    itimer::settime(pml4, id, t, start, rng.next() % 1000 * 1_000_000)
                        .map_err(|e| format!("round {round}: settime {e:?}"))?;
                }
            }
            4 => {
                if let Some(index) =
                    (!ids[which].is_empty()).then(|| (rng.next() as usize) % ids[which].len())
                {
                    let id = ids[which].swap_remove(index);
                    itimer::delete(pml4, id).map_err(|e| format!("round {round}: delete {e:?}"))?;
                }
            }
            _ => itimer::fire_due(t + rng.next() % (2 * NS)),
        }
        if round % 64 == 0 {
            itimer::check_for_test().map_err(|e| format!("round {round}: {e}"))?;
        }
    }
    for pml4 in spaces {
        itimer::forget(pml4);
    }
    itimer::check_for_test()?;
    check!(itimer::in_use() == 0, "{} timers leaked", itimer::in_use());
    check!(
        itimer::next_deadline().is_none(),
        "a deadline outlived its timers"
    );
    Ok(())
}

/// A 50 ms interval timer across two thousand periods fires once per
/// period, never more (no burst, no loss), and the CPU-time timers keep
/// pace over as many ticks.
pub fn many_ticks() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let real = spec((0, 50_000), (0, 50_000));
    check!(setitimer(0, &real, None) == 0, "setitimer REAL");
    let prof = spec((0, 10_000), (0, 10_000));
    check!(setitimer(2, &prof, None) == 0, "setitimer PROF");
    let start = now();
    let before = itimer::fired_total();
    for step in 1..=2000u64 {
        itimer::fire_due(start + step * 50_000_000);
    }
    let fired = itimer::fired_total() - before;
    check!(fired == 2000, "2000 periods sent {fired} signals");
    for _ in 0..2000 {
        task::itimer_tick_for_test(1, step_is_user());
    }
    let fired = itimer::fired_total() - before - 2000;
    check!(
        fired == 2000,
        "2000 ticks fired the PROF timer {fired} times"
    );
    itimer::check_for_test()?;
    finish(&[p])
}

/// Alternating user and kernel ticks: `ITIMER_PROF` counts both.
fn step_is_user() -> bool {
    static FLIP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    FLIP.fetch_xor(true, core::sync::atomic::Ordering::Relaxed)
}

/// Many processes that arm every kind of timer and exit (some without
/// disarming): the table and the signal registry return to where they were.
pub fn fork_exit_churn() -> Result<(), String> {
    fresh()?;
    let registry = signal::harness::registry_len();
    for round in 0..300u32 {
        let p = process().map_err(|e| format!("round {round}: {e}"))?;
        process::linux::dispatch_for_test(SYS_ALARM, 5, 0, 0);
        let t = spec((0, 5_000), (0, 5_000));
        check!(setitimer(1, &t, None) == 0, "round {round}: setitimer");
        check!(setitimer(2, &t, None) == 0, "round {round}: setitimer");
        for _ in 0..3 {
            let mut id = -1i32;
            let ret = process::linux::dispatch_for_test(SYS_TIMER_CREATE, 1, 0, ptr_mut(&mut id));
            check!(ret == 0, "round {round}: timer_create {ret:#x}");
            let arm = [0i64, 1_000_000, 0, 2_000_000];
            let ret = process::linux::dispatch_args_for_test(
                SYS_TIMER_SETTIME,
                id as u64,
                0,
                ptr(&arm),
                0,
            );
            check!(ret == 0, "round {round}: timer_settime {ret:#x}");
        }
        check!(
            itimer::in_use() == 6,
            "round {round}: {} timers",
            itimer::in_use()
        );
        // Every other round fires some before the process dies.
        if round % 2 == 0 {
            itimer::fire_due(now() + NS);
        }
        finish(&[p]).map_err(|e| format!("round {round}: {e}"))?;
        check!(
            itimer::in_use() == 0,
            "round {round}: {} timers outlived their process",
            itimer::in_use()
        );
    }
    itimer::check_for_test()?;
    check!(
        signal::harness::registry_len() == registry,
        "signal registry grew from {registry} to {}",
        signal::harness::registry_len()
    );
    check!(
        itimer::next_deadline().is_none(),
        "a deadline outlived its process"
    );
    Ok(())
}
