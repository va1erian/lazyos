//! `PROC:EXIT`: one line for every process that ends (issue #861).
//!
//! A freeze report used to say that something was stuck but never what died
//! or who killed it: a Linux process that exits or is killed left no line
//! unless a core signal crashed it. [`note_exit`] records the facts when the
//! process's last thread dies (`process::finish_locked`, the single place a
//! task dies, under the task-table lock), [`note_signal`] remembers who sent a
//! signal that is about to end a process, and [`report`] prints the lines from
//! the multiplexer loop, where neither the table nor the serial ring is held.
//!
//! A **process** is a thread group: the tasks whose `tgid` is the same (a
//! task's own slot for a leader). Not an address space: a `vfork` child shares
//! its parent's, and a thread that `execve`s leaves its siblings on the old
//! one. How a process died is what the kernel recorded for `wait4`
//! (`LinuxExtras::term_signal`), not a guess from the exit status: `exit(137)`
//! is a status, not `SIGKILL`.
//!
//! Both writers can run in interrupt context (the signal sweep, interval
//! timers), so the two small tables are locked with interrupts off only, and
//! the printer never holds one while it prints. A fork storm cannot bury the
//! exits that matter: clean exits have a budget of their own, and so do the
//! rest.

use core::fmt;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{cpustat, linuxstate, Task, TaskState, KERNEL_TASK, MAX_TASKS};

/// Exits kept between two reports; a burst past it is counted, not lost silently.
const EXITS: usize = 64;
/// Fatal-signal senders remembered (the newest replace the oldest).
const SENDERS: usize = 32;

/// Lines per ten seconds: clean exits (`exit(0)` of every shell command),
/// abnormal ones (a status, a signal), and the notice that exits were lost.
static CLEAN: tracefmt::Budget = tracefmt::Budget::new(20, 1000);
static ABNORMAL: tracefmt::Budget = tracefmt::Budget::new(100, 1000);
static LOST: tracefmt::Budget = tracefmt::Budget::new(5, 1000);

/// One ended process.
#[derive(Clone, Copy)]
struct Exit {
    tgid: usize,
    /// The thread whose death ended it.
    thread: usize,
    thread_name: &'static str,
    comm: [u8; 16],
    status: u64,
    /// The signal that ended it (0: it exited).
    sig: u8,
    parent: usize,
    cpu_ticks: u64,
    pml4: u64,
}

/// Who last sent a signal that can end process `pml4`.
#[derive(Clone, Copy)]
struct Sender {
    pml4: u64,
    sig: u8,
    by: usize,
}

struct Tables {
    exits: [Option<Exit>; EXITS],
    head: usize,
    len: usize,
    lost: u64,
    senders: [Option<Sender>; SENDERS],
    next_sender: usize,
}

static TABLES: Mutex<Tables> = Mutex::new(Tables {
    exits: [None; EXITS],
    head: 0,
    len: 0,
    lost: 0,
    senders: [None; SENDERS],
    next_sender: 0,
});

/// Remember that process `by` (a thread-group id; 0: the kernel) sent `sig`,
/// whose default action ends the process, to address space `pml4`. Call only
/// where the signal will in fact end it (not ignored, not caught).
pub fn note_signal(pml4: u64, sig: u8, by: usize) {
    without_interrupts(|| {
        let mut tables = TABLES.lock();
        let entry = Sender { pml4, sig, by };
        // The same (process, signal) again: refresh it in place.
        if let Some(existing) = tables
            .senders
            .iter_mut()
            .flatten()
            .find(|s| s.pml4 == pml4 && s.sig == sig)
        {
            *existing = entry;
            return;
        }
        let at = tables.next_sender;
        tables.senders[at] = Some(entry);
        tables.next_sender = (at + 1) % SENDERS;
    });
}

/// The thread-group id of `task`, which sits in `slot`.
fn tgid_of(task: &Task, slot: usize) -> usize {
    match task.linux.tgid {
        0 => slot,
        leader => leader,
    }
}

/// `slot` just became `Done` under the task-table lock: if it was the last
/// live thread of its process, record the exit. Never allocates.
pub(super) fn note_exit(tasks: &[Option<Task>; MAX_TASKS], slot: usize, status: u64) {
    if slot == KERNEL_TASK {
        return;
    }
    let Some(task) = tasks[slot].as_ref() else {
        return;
    };
    let tgid = tgid_of(task, slot);
    let mut cpu_ticks = 0u64;
    let mut sig = 0u8;
    for (index, other) in tasks.iter().enumerate() {
        let Some(other) = other else {
            continue;
        };
        if index == KERNEL_TASK || tgid_of(other, index) != tgid {
            continue;
        }
        if other.state != TaskState::Done {
            return; // another thread of the process still runs
        }
        cpu_ticks = cpu_ticks.saturating_add(other.cpu_ticks);
        if sig == 0 {
            sig = other.linux.term_signal;
        }
    }
    cpu_ticks = cpu_ticks.saturating_add(cpustat::take_retired(tgid));
    // The leader names the parent, if it is still the process's own leader
    // (a recycled slot is not).
    let leader = tasks
        .get(tgid)
        .and_then(|l| l.as_ref())
        .filter(|l| tgid_of(l, tgid) == tgid && l.pml4 == task.pml4);
    let parent_slot = leader.map_or(task.parent, |l| l.parent);
    let parent = tasks
        .get(parent_slot)
        .and_then(|p| p.as_ref())
        .map_or(parent_slot, |p| tgid_of(p, parent_slot));
    let (comm, _) = linuxstate::comm_of(task);
    let exit = Exit {
        tgid,
        thread: slot,
        thread_name: task.name,
        comm,
        status,
        sig,
        parent,
        cpu_ticks,
        pml4: task.pml4,
    };
    without_interrupts(|| {
        let mut tables = TABLES.lock();
        // Whoever signalled this address space is history now: a recycled
        // page table must not inherit the sender.
        let pml4 = task.pml4;
        for sender in tables.senders.iter_mut() {
            if sender.is_some_and(|s| s.pml4 == pml4 && s.sig != sig) {
                *sender = None;
            }
        }
        if tables.len == EXITS && !make_room(&mut tables, exit.sig == 0 && exit.status == 0) {
            return; // a clean exit with nothing clean to replace: lost
        }
        let at = (tables.head + tables.len) % EXITS;
        tables.exits[at] = Some(exit);
        tables.len += 1;
    });
}

/// A full table needs room for one more exit: drop the oldest *clean* exit
/// (a shell loop's `exit(0)`), and only if there is none the oldest of all, so
/// a storm of clean exits never evicts a crash. A clean newcomer finding only
/// abnormal exits is itself the one lost (returns false). Counts the loss.
fn make_room(tables: &mut Tables, newcomer_clean: bool) -> bool {
    let clean = |exit: &Option<Exit>| exit.is_some_and(|e| e.sig == 0 && e.status == 0);
    let found = (0..tables.len).find(|&k| clean(&tables.exits[(tables.head + k) % EXITS]));
    tables.lost += 1;
    match found {
        Some(k) => {
            // Shift the entries after `k` down over it.
            for i in k..tables.len - 1 {
                let from = (tables.head + i + 1) % EXITS;
                let to = (tables.head + i) % EXITS;
                tables.exits[to] = tables.exits[from];
            }
            tables.len -= 1;
            true
        }
        None if newcomer_clean => false,
        None => {
            tables.head = (tables.head + 1) % EXITS;
            tables.len -= 1;
            true
        }
    }
}

/// A value that prints as `-` when absent.
struct Opt(Option<usize>);

impl fmt::Display for Opt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(f, "{value}"),
            None => f.write_str("-"),
        }
    }
}

/// One exit, ready to print.
struct Line {
    exit: Exit,
    by: Option<usize>,
}

/// Print the exits recorded since the last call. Task context (the kernel
/// task's loop); takes no lock while printing.
pub fn report() {
    let mut lines: [Option<Line>; EXITS] = [const { None }; EXITS];
    let mut count = 0;
    let lost = without_interrupts(|| {
        let mut tables = TABLES.lock();
        while tables.len > 0 {
            let at = tables.head;
            let Some(exit) = tables.exits[at].take() else {
                break;
            };
            tables.head = (at + 1) % EXITS;
            tables.len -= 1;
            // The sender of the signal that ended it; consumed, with every
            // other entry of the address space (`note_exit` kept the one
            // that matters).
            let mut by = None;
            let pml4 = exit.pml4;
            for sender in tables.senders.iter_mut() {
                if let Some(s) = *sender {
                    if s.pml4 == pml4 {
                        if exit.sig != 0 && s.sig == exit.sig {
                            by = Some(s.by);
                        }
                        *sender = None;
                    }
                }
            }
            lines[count] = Some(Line { exit, by });
            count += 1;
        }
        core::mem::take(&mut tables.lost)
    });
    let now = super::ticks();
    for line in lines.iter().flatten() {
        let clean = line.exit.sig == 0 && line.exit.status == 0;
        let budget = if clean { &CLEAN } else { &ABNORMAL };
        match budget.charge(now) {
            tracefmt::Charge::Deny => continue,
            tracefmt::Charge::AllowAfter(n) => {
                let kind = if clean { "clean" } else { "other" };
                crate::trace::line!(tracefmt::Channel::Proc, "SUPPRESSED {kind}={n}");
            }
            tracefmt::Charge::Allow => {}
        }
        print(line);
    }
    if lost > 0 {
        match LOST.charge(now) {
            tracefmt::Charge::Deny => {}
            tracefmt::Charge::Allow => {
                crate::trace::line!(tracefmt::Channel::Proc, "LOST exits={lost}");
            }
            tracefmt::Charge::AllowAfter(n) => {
                crate::trace::line!(
                    tracefmt::Channel::Proc,
                    "LOST exits={lost} notices_suppressed={n}"
                );
            }
        }
    }
}

fn print(line: &Line) {
    let exit = &line.exit;
    let len = exit.comm.iter().position(|&b| b == 0).unwrap_or(16);
    let comm = core::str::from_utf8(&exit.comm[..len]).unwrap_or("?");
    // A process killed by a signal has no exit status; one that exited has no
    // signal. `by=0` is the kernel (a fault, a timer).
    let signalled = exit.sig != 0;
    let status = Opt((!signalled).then_some(exit.status as usize));
    let sig = Opt(signalled.then_some(usize::from(exit.sig)));
    let by = match line.by {
        Some(0) => String::from("kernel"),
        by => alloc::format!("{}", Opt(by)),
    };
    crate::trace::line!(
        tracefmt::Channel::Proc,
        "EXIT tgid={} thread={}/{} status={} sig={} by={} parent={} cpu_ms={} comm={}",
        exit.tgid,
        exit.thread,
        exit.thread_name,
        status,
        sig,
        by,
        exit.parent,
        exit.cpu_ticks * 10,
        comm
    );
}

use alloc::string::String;

/// Drop everything recorded (the kernel suite).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    without_interrupts(|| {
        let mut tables = TABLES.lock();
        tables.exits = [None; EXITS];
        tables.head = 0;
        tables.len = 0;
        tables.lost = 0;
        tables.senders = [None; SENDERS];
        tables.next_sender = 0;
    });
    CLEAN.reset();
    ABNORMAL.reset();
    LOST.reset();
}

/// Fresh line budgets (the suite).
#[cfg(lazyos_tests)]
pub fn reset_budgets_for_test() {
    CLEAN.reset();
    ABNORMAL.reset();
    LOST.reset();
}

/// Exits waiting to be printed and exits lost to a full table (the suite).
#[cfg(lazyos_tests)]
pub fn pending_for_test() -> (usize, u64) {
    without_interrupts(|| {
        let tables = TABLES.lock();
        (tables.len, tables.lost)
    })
}

/// Senders remembered (the suite): none should outlive their process.
#[cfg(lazyos_tests)]
pub fn senders_for_test() -> usize {
    without_interrupts(|| TABLES.lock().senders.iter().flatten().count())
}
