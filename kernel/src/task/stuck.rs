//! `SCHED:STUCK`: a task that stays runnable inside the kernel (issue #861).
//!
//! A thread that loops in a syscall (issue #857's console write waiting for
//! a serial ring that never emptied) is invisible: it is `Runnable`, the
//! guard demotes it, and nothing says it is not running user code. The
//! multiplexer loop samples the table; a task that is `Runnable` and inside
//! one kernel invocation, the same at every sample for [`STUCK_TICKS`], is
//! reported, and again every [`REPEAT_TICKS`] while it lasts. `cpu_ms` tells a
//! task that spins (it ran) from one that is starved (it did not).
//!
//! "Inside the kernel" is judged from the kernel stack, not from the saved
//! frame alone: an interrupt stub's `preempt_point` switches a task out (in
//! `yield_now`) with a *kernel* frame saved while the task was only
//! interrupted in user mode, and the frame the CPU pushed at the top of the
//! stack is the ring-3 one. Such a task is not stuck; a task in a syscall
//! (`irqoff::task_syscall`), whose outermost frame is a kernel one, or that
//! was preempted by the timer inside a handler it entered from user mode (it
//! is not in `yield_now`) is.
//!
//! A task blocked in a syscall is not reported (a `read` may wait for
//! minutes); neither is one in user mode.

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{linuxstate, signal, TaskState, MAX_TASKS, TASKS};

/// Ticks (100 Hz) a task must stay in one kernel invocation, runnable.
const STUCK_TICKS: u64 = 200;
/// Ticks between two reports of the same task.
const REPEAT_TICKS: u64 = 1000;
/// Reports one sample prints; the rest come with the next.
const ROWS_MAX: usize = 8;

/// Lines allowed in a ten-second window, all tasks together: a machine with
/// many stuck tasks says so, once in a while, instead of 80 lines a second.
static BUDGET: tracefmt::Budget = tracefmt::Budget::new(16, 1000);

/// What was seen of one slot at the last sample.
#[derive(Clone, Copy)]
struct Watch {
    /// The kernel invocation (syscall number and entry tick, or 1 for a
    /// kernel frame outside a syscall); 0 when the task is not in one.
    key: u64,
    /// Tick the key was first seen at, and the task's `cpu_ticks` then.
    since: u64,
    cpu0: u64,
    /// Tick of the last report (0: none).
    printed: u64,
}

const IDLE: Watch = Watch {
    key: 0,
    since: 0,
    cpu0: 0,
    printed: 0,
};

static WATCH: Mutex<[Watch; MAX_TASKS]> = Mutex::new([IDLE; MAX_TASKS]);

/// One task to report.
#[derive(Clone, Copy)]
struct Row {
    slot: usize,
    tgid: usize,
    name: &'static str,
    comm: [u8; 16],
    call: Option<(bool, u64)>,
    /// Ticks in the invocation (since the syscall was entered, else since
    /// first seen) and CPU ticks the task ran meanwhile.
    ticks: u64,
    cpu_ticks: u64,
    krip: u64,
    urip: u64,
}

/// Look at every task once; print the ones stuck. Task context, interrupts off.
pub fn sample() {
    sample_at(super::ticks());
}

fn sample_at(now: u64) {
    let mut rows = [None::<Row>; ROWS_MAX];
    let mut count = 0;
    without_interrupts(|| {
        let tasks = TASKS.lock();
        let mut watch = WATCH.lock();
        let me = super::current();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                watch[slot] = IDLE;
                continue;
            };
            // A task current on some CPU (this one, or another under S8.3c)
            // is running: its saved frame is stale and its stack is live.
            let on_cpu = slot == me || super::oncpu::cpu_of(slot).is_some();
            if task.state != TaskState::Runnable || on_cpu || task.kstack_top < 64 {
                watch[slot] = IDLE;
                continue;
            }
            let call = crate::arch::irqoff::task_syscall(slot);
            // SAFETY: `task` is a live task current on no CPU, so
            // its `rsp` is the frame the scheduler saved and nothing writes
            // its kernel stack while the table is held with interrupts off.
            let (krip, saved_user) = unsafe { signal::saved_rip(task.rsp) };
            let (urip, key) = match call {
                Some((native, nr, entered)) => {
                    // SAFETY: as above; `kstack_top` is this task's.
                    let (urip, _) = unsafe { signal::outer_user_frame(task.kstack_top, !native) };
                    (
                        urip,
                        (u64::from(native) << 63) ^ (nr << 40) ^ entered.wrapping_add(2),
                    )
                }
                None if saved_user => (0, 0),
                None => {
                    // SAFETY: as above.
                    let (urip, user) = unsafe { signal::outer_user_frame(task.kstack_top, false) };
                    // Interrupted in user mode and switched out by a stub's
                    // `preempt_point` (it is inside `yield_now`): on its way
                    // back, not stuck. Preempted by the timer while it ran
                    // kernel code for that interrupt or exception (a #PF
                    // handler that loops), it is.
                    (
                        urip,
                        if user && super::switch::yielding(slot) {
                            0
                        } else {
                            1
                        },
                    )
                }
            };
            let entry = &mut watch[slot];
            if key == 0 {
                *entry = IDLE;
                continue;
            }
            if entry.key != key {
                *entry = Watch {
                    key,
                    since: now,
                    cpu0: task.cpu_ticks,
                    printed: 0,
                };
                continue;
            }
            let age = now.wrapping_sub(entry.since);
            let due = entry.printed == 0 && age >= STUCK_TICKS
                || entry.printed != 0 && now.wrapping_sub(entry.printed) >= REPEAT_TICKS;
            if !due || count == ROWS_MAX {
                continue;
            }
            entry.printed = now;
            let tgid = match task.linux.tgid {
                0 => slot,
                leader => leader,
            };
            let ticks = match call {
                Some((_, _, entered)) => now.saturating_sub(entered),
                None => age,
            };
            rows[count] = Some(Row {
                slot,
                tgid,
                name: task.name,
                comm: linuxstate::comm_of(task).0,
                call: call.map(|(native, nr, _)| (native, nr)),
                ticks,
                cpu_ticks: task.cpu_ticks.saturating_sub(entry.cpu0),
                krip,
                urip,
            });
            count += 1;
        }
    });
    for row in rows.iter().flatten() {
        match BUDGET.charge(now) {
            tracefmt::Charge::Deny => continue,
            tracefmt::Charge::AllowAfter(n) => {
                crate::trace::line!(tracefmt::Channel::Sched, "STUCK_SUPPRESSED n={n}")
            }
            tracefmt::Charge::Allow => {}
        }
        print(row);
    }
}

fn print(row: &Row) {
    let len = row.comm.iter().position(|&b| b == 0).unwrap_or(16);
    let comm = core::str::from_utf8(&row.comm[..len]).unwrap_or("?");
    let (abi, nr) = match row.call {
        Some((true, nr)) => ("native", nr as i64),
        Some((false, nr)) => ("linux", nr as i64),
        None => ("-", -1),
    };
    crate::trace::line!(
        tracefmt::Channel::Sched,
        "STUCK tgid={} thread={}/{} abi={} nr={} ms={} cpu_ms={} krip={:#x} urip={:#x} comm={}",
        row.tgid,
        row.slot,
        row.name,
        abi,
        nr,
        row.ticks * 10,
        row.cpu_ticks * 10,
        row.krip,
        row.urip,
        comm
    );
}

/// Forget every watch and the line budget (the kernel suite).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    without_interrupts(|| *WATCH.lock() = [IDLE; MAX_TASKS]);
    BUDGET.reset();
}

/// Whether the slot is being watched as in a kernel invocation, with the
/// tick it was first seen at and whether it was reported (the suite).
#[cfg(lazyos_tests)]
pub fn watched_for_test(slot: usize) -> Option<(u64, bool)> {
    without_interrupts(|| {
        let watch = WATCH.lock();
        (watch[slot].key != 0).then_some((watch[slot].since, watch[slot].printed != 0))
    })
}

/// [`sample`] at a chosen tick (the suite): same rules, a fake clock.
#[cfg(lazyos_tests)]
pub fn sample_at_for_test(now: u64) {
    sample_at(now);
}
