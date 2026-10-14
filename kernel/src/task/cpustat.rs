//! `PERF:CPU`: which processes use the CPU (issue #861).
//!
//! Every [`PERIOD_TICKS`] the multiplexer loop adds up, per address space, the
//! ticks its threads were charged since the last look. When any process is
//! over [`BUSY_PERCENT`], the top [`TOP`] are printed with the user/kernel
//! split of their ticks, their class and their demotions, so a freeze report
//! says who was busy and in which half of the system.

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{linuxstate, PriorityClass, TASKS};

/// Ticks between two looks (5 s).
const PERIOD_TICKS: u64 = 500;
/// A process above this share of the CPU makes the sample print.
const BUSY_PERCENT: u64 = 20;
/// Processes printed.
const TOP: usize = 5;
/// Processes tracked; the rest of a very crowded machine is not counted.
const PROCESSES: usize = 96;

/// One address space's totals at a look.
#[derive(Clone, Copy)]
struct Usage {
    pml4: u64,
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
                let before = state
                    .previous
                    .iter()
                    .flatten()
                    .find(|p| p.pml4 == usage.pml4)
                    .map_or((usage.ticks, usage.user), |p| (p.ticks, p.user));
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

/// Totals per address space over the whole table (kernel task excluded).
fn totals() -> [Option<Usage>; PROCESSES] {
    let tasks = TASKS.lock();
    let mut out = [None::<Usage>; PROCESSES];
    for (slot, entry) in tasks.iter().enumerate().skip(1) {
        let Some(task) = entry else {
            continue;
        };
        let at = out
            .iter()
            .position(|u| u.is_some_and(|u| u.pml4 == task.pml4))
            .or_else(|| out.iter().position(Option::is_none));
        let Some(at) = at else {
            continue;
        };
        let tgid = match task.linux.tgid {
            0 => slot,
            leader => leader,
        };
        let usage = out[at].get_or_insert(Usage {
            pml4: task.pml4,
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
            usage.tgid = tgid;
            usage.comm = linuxstate::comm_of(task).0;
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

/// Forget the previous look (the kernel suite).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    without_interrupts(|| {
        let mut state = STATE.lock();
        state.at = 0;
        state.previous = [None; PROCESSES];
    });
}

/// [`sample`] at a chosen tick (the suite).
#[cfg(lazyos_tests)]
pub fn sample_at_for_test(now: u64) {
    sample_at(now);
}

/// Table slots this module can track (the suite).
#[cfg(lazyos_tests)]
pub const CAPACITY: usize = PROCESSES;
