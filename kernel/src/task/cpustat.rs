//! `PERF:CPU`: which processes use the CPU (issue #861).
//!
//! Every [`PERIOD_TICKS`] the multiplexer loop adds up, per process (thread
//! group), the ticks its threads were charged since the last look. When any
//! process is over [`BUSY_PERCENT`], the top [`TOP`] are printed with the
//! user/kernel split of their ticks, their class and their demotions, so a
//! freeze report says who was busy and in which half of the system.
//!
//! A thread's ticks leave the table with the thread when it is reaped, so a
//! process that spawns and ends threads would look idle (its sum shrinks). A
//! reaped thread's ticks are therefore added to a per-process accumulator
//! ([`retire`]) that the sums include, until the process itself ends
//! ([`take_retired`], by `PROC:EXIT`).

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{linuxstate, PriorityClass, Task, TASKS};

/// Ticks between two looks (5 s).
const PERIOD_TICKS: u64 = 500;
/// A process above this share of the CPU makes the sample print.
const BUSY_PERCENT: u64 = 20;
/// Processes printed.
const TOP: usize = 5;
/// Processes tracked; the rest of a very crowded machine is not counted.
const PROCESSES: usize = 96;

/// Lines per ten seconds, all processes together.
static BUDGET: tracefmt::Budget = tracefmt::Budget::new(15, 1000);

/// One process's totals at a look.
#[derive(Clone, Copy)]
struct Usage {
    tgid: usize,
    comm: [u8; 16],
    ticks: u64,
    user: u64,
    class: PriorityClass,
    demotions: u64,
}

struct State {
    at: u64,
    previous: [Option<Usage>; PROCESSES],
}

static STATE: Mutex<State> = Mutex::new(State {
    at: 0,
    previous: [None; PROCESSES],
});

/// Ticks of reaped threads, per process: `(tgid, ticks, user ticks)`.
static RETIRED: Mutex<[Option<(usize, u64, u64)>; PROCESSES]> = Mutex::new([None; PROCESSES]);

/// A thread (`task`, in `slot`) is being reaped: keep its ticks with its
/// process. Call with the task table locked (the lock order is table, then
/// this); the reaped task has just been taken out of it. A process with no
/// live thread left is over (`PROC:EXIT` took its ticks): nothing to keep.
pub(super) fn retire(tasks: &[Option<Task>; super::MAX_TASKS], slot: usize, task: &Task) {
    if task.cpu_ticks == 0 {
        return;
    }
    let tgid = match task.linux.tgid {
        0 => slot,
        leader => leader,
    };
    let alive = tasks.iter().enumerate().any(|(index, other)| {
        other.as_ref().is_some_and(|other| {
            other.state != super::TaskState::Done
                && match other.linux.tgid {
                    0 => index,
                    leader => leader,
                } == tgid
        })
    });
    if !alive {
        return;
    }
    let user = task.guard.user_ticks();
    without_interrupts(|| {
        let mut retired = RETIRED.lock();
        let at = retired
            .iter()
            .position(|e| e.is_some_and(|e| e.0 == tgid))
            .or_else(|| retired.iter().position(Option::is_none));
        // A full table loses these ticks: the sum shrinks, which only makes a
        // busy process look a little less busy.
        if let Some(at) = at {
            let (_, ticks, users) = retired[at].unwrap_or((tgid, 0, 0));
            retired[at] = Some((tgid, ticks.saturating_add(task.cpu_ticks), users + user));
        }
    });
}

/// The reaped threads' ticks of process `tgid`, forgotten (the process ended).
pub(super) fn take_retired(tgid: usize) -> u64 {
    without_interrupts(|| {
        let mut retired = RETIRED.lock();
        for entry in retired.iter_mut() {
            if entry.is_some_and(|e| e.0 == tgid) {
                return entry.take().map_or(0, |e| e.1);
            }
        }
        0
    })
}

/// Look, and print if a process is busy. Task context, interrupts off.
pub fn sample() {
    sample_at(super::ticks());
}

fn sample_at(now: u64) {
    let mut top = [None::<(u64, Usage, u64)>; TOP];
    let busy = without_interrupts(|| {
        let mut state = STATE.lock();
        if now.wrapping_sub(state.at) < PERIOD_TICKS {
            return false;
        }
        let elapsed = now.wrapping_sub(state.at);
        let first = state.at == 0;
        state.at = now;
        let current = totals();
        let mut busy = false;
        if !first {
            for usage in current.iter().flatten() {
                // A process not seen at the last look began since: all its
                // ticks are this period's.
                let before = state
                    .previous
                    .iter()
                    .flatten()
                    .find(|p| p.tgid == usage.tgid)
                    .map_or((0, 0), |p| (p.ticks, p.user));
                let delta = usage.ticks.saturating_sub(before.0);
                let user = usage.user.saturating_sub(before.1);
                let percent = delta * 100 / elapsed;
                if percent >= BUSY_PERCENT {
                    busy = true;
                }
                insert(&mut top, percent, *usage, user * 100 / elapsed);
            }
        }
        state.previous = current;
        busy
    });
    if !busy {
        return;
    }
    for (percent, usage, user) in top.iter().flatten() {
        match BUDGET.charge(now) {
            tracefmt::Charge::Deny => continue,
            tracefmt::Charge::AllowAfter(n) => {
                crate::trace::line!(tracefmt::Channel::Perf, "CPU_SUPPRESSED n={n}")
            }
            tracefmt::Charge::Allow => {}
        }
        print(*percent, usage, *user);
    }
}

/// Keep the [`TOP`] highest percentages, highest first.
fn insert(top: &mut [Option<(u64, Usage, u64)>; TOP], percent: u64, usage: Usage, user: u64) {
    if percent == 0 {
        return;
    }
    let mut candidate = Some((percent, usage, user));
    for place in top.iter_mut() {
        match (&candidate, &*place) {
            (Some((p, _, _)), Some((q, _, _))) if p > q => core::mem::swap(place, &mut candidate),
            (Some(_), None) => {
                *place = candidate.take();
                return;
            }
            _ => {}
        }
    }
}

/// Totals per process over the whole table (kernel task excluded), reaped
/// threads included.
fn totals() -> [Option<Usage>; PROCESSES] {
    let tasks = TASKS.lock();
    let mut out = [None::<Usage>; PROCESSES];
    for (slot, entry) in tasks.iter().enumerate().skip(1) {
        let Some(task) = entry else {
            continue;
        };
        let tgid = match task.linux.tgid {
            0 => slot,
            leader => leader,
        };
        let at = out
            .iter()
            .position(|u| u.is_some_and(|u| u.tgid == tgid))
            .or_else(|| out.iter().position(Option::is_none));
        let Some(at) = at else {
            continue;
        };
        let usage = out[at].get_or_insert(Usage {
            tgid,
            comm: linuxstate::comm_of(task).0,
            ticks: 0,
            user: 0,
            class: task.guard.home(task.class),
            demotions: 0,
        });
        usage.ticks = usage.ticks.saturating_add(task.cpu_ticks);
        usage.user = usage.user.saturating_add(task.guard.user_ticks());
        usage.demotions += u64::from(task.guard.demotions());
        let home = task.guard.home(task.class);
        if home.default_weight() > usage.class.default_weight() {
            usage.class = home;
        }
        // The leader names the process.
        if slot == tgid {
            usage.comm = linuxstate::comm_of(task).0;
        }
    }
    for usage in out.iter_mut().flatten() {
        if let Some((_, ticks, user)) = RETIRED.lock().iter().flatten().find(|e| e.0 == usage.tgid)
        {
            usage.ticks = usage.ticks.saturating_add(*ticks);
            usage.user = usage.user.saturating_add(*user);
        }
    }
    out
}

fn print(percent: u64, usage: &Usage, user: u64) {
    let len = usage.comm.iter().position(|&b| b == 0).unwrap_or(16);
    let comm = core::str::from_utf8(&usage.comm[..len]).unwrap_or("?");
    crate::trace::line!(
        tracefmt::Channel::Perf,
        "CPU tgid={} pct={} user={} kernel={} class={} demotions={} comm={}",
        usage.tgid,
        percent,
        user.min(percent),
        percent.saturating_sub(user),
        usage.class.label(),
        usage.demotions,
        comm
    );
}

/// Forget the previous look and the line budget (the kernel suite).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    without_interrupts(|| {
        let mut state = STATE.lock();
        state.at = 0;
        state.previous = [None; PROCESSES];
        *RETIRED.lock() = [None; PROCESSES];
    });
    BUDGET.reset();
}

/// [`sample`] at a chosen tick (the suite).
#[cfg(lazyos_tests)]
pub fn sample_at_for_test(now: u64) {
    sample_at(now);
}

/// Table slots this module can track (the suite).
#[cfg(lazyos_tests)]
pub const CAPACITY: usize = PROCESSES;

/// Ticks kept for reaped threads of process `tgid` (the suite).
#[cfg(lazyos_tests)]
pub fn retired_for_test(tgid: usize) -> u64 {
    without_interrupts(|| {
        RETIRED
            .lock()
            .iter()
            .flatten()
            .find(|e| e.0 == tgid)
            .map_or(0, |e| e.1)
    })
}
