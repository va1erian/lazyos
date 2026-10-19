//! `PERF:BLOCKED`: where the threads of the busiest processes spend their time when they do not run
//! (`LAZYOS_PERF=1` images).
//!
//! `PERF:CPU` says how much a process ran; a process whose work is slow while the CPU is idle is
//! waiting, and this says on what. Ten times a second (the forensics sample) every thread is classed by
//! its state: running or runnable, or blocked in a syscall on a kind of wait (`futex`, `poll`, `pipe`,
//! ...). Every five seconds the commonest (thread, syscall, wait) triples are printed with how many of the
//! fifty samples they took:
//!
//! ```text
//! PERF:BLOCKED tgid=<n> slot=<n> nr=<linux:n, native:n or -> wait=<kind or run> n=<samples of 50> comm=<thread>
//! ```
//!
//! Only threads of processes named in [`WATCHED`] are counted (the browser's three processes).
//! A Messenger wait (`recv`, `wait_any`) shows as `sleep`: that is the kind of its wait queue.

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{linuxstate, TaskState, WaitKind, MAX_TASKS, TASKS};

/// Processes whose threads are sampled, by the name of their first thread.
const WATCHED: &[&[u8]] = &[b"webprocess", b"networkprocess", b"lazywebplus.elf"];
/// Samples between two reports (the forensics sample runs every 10 ticks: five seconds).
const PERIOD: u32 = 50;
/// Distinct (thread, syscall, wait) entries kept, and printed.
const ENTRIES: usize = 96;
const ROWS: usize = 24;

#[derive(Clone, Copy)]
struct Entry {
    slot: u16,
    tgid: u16,
    /// Syscall number, or `u16::MAX` outside one.
    nr: u16,
    /// Whether `nr` is a native syscall (else a Linux one).
    native: bool,
    /// [`WaitKind`] as a small number, or 255 for running/runnable.
    wait: u8,
    samples: u32,
    comm: [u8; 16],
}

struct State {
    samples: u32,
    entries: [Option<Entry>; ENTRIES],
}

static STATE: Mutex<State> = Mutex::new(State {
    samples: 0,
    entries: [None; ENTRIES],
});

fn wait_code(wait: WaitKind) -> u8 {
    match wait {
        WaitKind::Futex => 0,
        WaitKind::Terminal => 1,
        WaitKind::ChildExit => 2,
        WaitKind::Sleep => 3,
        WaitKind::Pipe => 4,
        WaitKind::UnixAccept => 5,
        WaitKind::Poll => 6,
        WaitKind::Signal => 7,
        WaitKind::Slot => 8,
        WaitKind::Block => 9,
    }
}

fn wait_name(code: u8) -> &'static str {
    match code {
        0 => "futex",
        1 => "terminal",
        2 => "child",
        3 => "sleep",
        4 => "pipe",
        5 => "accept",
        6 => "poll",
        7 => "signal",
        8 => "slot",
        9 => "block",
        _ => "run",
    }
}

/// One look at the table; every [`PERIOD`] looks, the report. Task context, interrupts off.
pub fn sample() {
    let report = without_interrupts(|| {
        let tasks = TASKS.lock();
        let mut state = STATE.lock();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            let tgid = match task.linux.tgid {
                0 => slot,
                leader => leader,
            };
            let Some(leader) = tasks.get(tgid).and_then(Option::as_ref) else {
                continue;
            };
            let (name, len) = linuxstate::comm_of(leader);
            if !WATCHED.iter().any(|watched| &name[..len] == *watched) {
                continue;
            }
            let wait = match task.state {
                TaskState::Blocked { wait, .. } => wait_code(wait),
                TaskState::Runnable => 255,
                TaskState::Done => continue,
            };
            let (native, nr) = crate::arch::irqoff::task_syscall(slot).map_or((false, u16::MAX), |(native, nr, _)| (native, nr as u16));
            let comm = linuxstate::comm_of(task).0;
            let found = state.entries.iter_mut().flatten().find(|entry| {
                usize::from(entry.slot) == slot && entry.nr == nr && entry.native == native && entry.wait == wait
            });
            match found {
                Some(entry) => entry.samples += 1,
                None => {
                    if let Some(free) = state.entries.iter_mut().find(|entry| entry.is_none()) {
                        *free = Some(Entry {
                            slot: slot as u16,
                            tgid: tgid as u16,
                            nr,
                            native,
                            wait,
                            samples: 1,
                            comm,
                        });
                    }
                }
            }
        }
        state.samples += 1;
        if state.samples < PERIOD {
            return None;
        }
        let entries = state.entries;
        state.samples = 0;
        state.entries = [None; ENTRIES];
        Some(entries)
    });
    if let Some(mut entries) = report {
        entries.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.map_or(0, |e| e.samples)));
        // A helper thread parked for the whole period says nothing; the main threads always count.
        let telling = |entry: &&Entry| entry.samples < PERIOD || entry.slot == entry.tgid;
        for entry in entries.iter().flatten().filter(telling).take(ROWS) {
            print(entry);
        }
    }
}

fn print(entry: &Entry) {
    let len = entry.comm.iter().position(|&b| b == 0).unwrap_or(16);
    let comm = core::str::from_utf8(&entry.comm[..len]).unwrap_or("?");
    let (tgid, slot, samples) = (entry.tgid, entry.slot, entry.samples);
    let wait = wait_name(entry.wait);
    if entry.nr == u16::MAX {
        crate::trace::line!(
            tracefmt::Channel::Perf,
            "BLOCKED tgid={tgid} slot={slot} nr=- wait={wait} n={samples} comm={comm}"
        );
    } else {
        let (abi, nr) = (if entry.native { "native" } else { "linux" }, entry.nr);
        crate::trace::line!(
            tracefmt::Channel::Perf,
            "BLOCKED tgid={tgid} slot={slot} nr={abi}:{nr} wait={wait} n={samples} comm={comm}"
        );
    }
}
