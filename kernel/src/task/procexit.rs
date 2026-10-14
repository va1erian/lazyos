//! `PROC:EXIT`: one line for every process that ends (issue #861).
//!
//! A freeze report used to say that something was stuck but never what died
//! or who killed it: a Linux process that exits or is killed left no line
//! unless a core signal crashed it. [`note_exit`] records the facts when the
//! process's last thread dies (`process::finish_locked`, the single place a
//! task dies, under the task-table lock), [`note_signal`] remembers who sent a
//! fatal signal, and [`report`] prints the lines from the multiplexer loop,
//! where neither the table nor the serial ring is held.
//!
//! Both writers can run in interrupt context (the signal sweep, interval
//! timers), so the two small tables are locked with interrupts off only, and
//! the printer never holds one while it prints.

use core::fmt;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{linuxstate, Task, TaskState, KERNEL_TASK, MAX_TASKS};

/// Exits kept between two reports; a burst past it is counted, not lost silently.
const EXITS: usize = 64;
/// Fatal-signal senders remembered (the newest replace the oldest).
const SENDERS: usize = 32;

/// One ended process.
#[derive(Clone, Copy)]
struct Exit {
    tgid: usize,
    comm: [u8; 16],
    status: u64,
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

/// Remember that `by` (the sender's process id) sent `sig` to process `pml4`.
/// Only signals that end a process by default matter; the rest are ignored.
pub fn note_signal(pml4: u64, sig: u8, by: usize) {
    if !ends_process(sig) {
        return;
    }
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

/// Signals whose default action ends the process (the ones `128 + sig`
/// statuses come from).
fn ends_process(sig: u8) -> bool {
    matches!(
        sig,
        1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 24 | 25 | 26 | 27 | 31
    )
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
    let pml4 = task.pml4;
    let mut cpu_ticks = 0u64;
    for other in tasks.iter().flatten() {
        if other.pml4 != pml4 {
            continue;
        }
        if other.state != TaskState::Done {
            return; // another thread still runs: the process lives
        }
        cpu_ticks = cpu_ticks.saturating_add(other.cpu_ticks);
    }
    let tgid = match task.linux.tgid {
        0 => slot,
        leader => leader,
    };
    let parent_slot = tasks
        .get(tgid)
        .and_then(|leader| leader.as_ref())
        .map_or(task.parent, |leader| leader.parent);
    let parent = match tasks.get(parent_slot).and_then(|p| p.as_ref()) {
        Some(parent) => match parent.linux.tgid {
            0 => parent_slot,
            leader => leader,
        },
        None => parent_slot,
    };
    let (comm, _) = linuxstate::comm_of(task);
    let exit = Exit {
        tgid,
        comm,
        status,
        parent,
        cpu_ticks,
        pml4,
    };
    without_interrupts(|| {
        let mut tables = TABLES.lock();
        if tables.len == EXITS {
            tables.head = (tables.head + 1) % EXITS;
            tables.len -= 1;
            tables.lost += 1;
        }
        let at = (tables.head + tables.len) % EXITS;
        tables.exits[at] = Some(exit);
        tables.len += 1;
    });
}

/// The signal a status stands for (`128 + sig`), if it is one.
fn signal_of(status: u64) -> Option<u8> {
    (129..=128 + 64)
        .contains(&status)
        .then(|| (status - 128) as u8)
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
            let by = signal_of(exit.status).and_then(|sig| {
                tables
                    .senders
                    .iter_mut()
                    .find(|s| s.is_some_and(|s| s.pml4 == exit.pml4 && s.sig == sig))
                    .and_then(|s| s.take())
                    .map(|s| s.by)
            });
            lines[count] = Some(Line { exit, by });
            count += 1;
        }
        core::mem::take(&mut tables.lost)
    });
    for line in lines.iter().flatten() {
        print(line);
    }
    if lost > 0 {
        crate::trace::line!(tracefmt::Channel::Proc, "LOST exits={lost}");
    }
}

fn print(line: &Line) {
    let exit = &line.exit;
    let len = exit.comm.iter().position(|&b| b == 0).unwrap_or(16);
    let comm = core::str::from_utf8(&exit.comm[..len]).unwrap_or("?");
    let sig = signal_of(exit.status).map(usize::from);
    let status = if sig.is_some() { 0 } else { exit.status };
    crate::trace::line!(
        tracefmt::Channel::Proc,
        "EXIT tgid={} status={} sig={} by={} parent={} cpu_ms={} comm={}",
        exit.tgid,
        status,
        Opt(sig),
        Opt(line.by),
        exit.parent,
        exit.cpu_ticks * 10,
        comm
    );
}

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
}

/// Exits waiting to be printed and exits lost to a full table (the suite).
#[cfg(lazyos_tests)]
pub fn pending_for_test() -> (usize, u64) {
    without_interrupts(|| {
        let tables = TABLES.lock();
        (tables.len, tables.lost)
    })
}
