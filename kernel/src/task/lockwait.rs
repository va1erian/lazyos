//! `LOCK:WAIT`: a task that has waited long for a contended lock (issue #861).
//!
//! The filesystem tables and an ext2 volume are `YieldMutex`es: a contender
//! parks in short slices while the holder (often parked itself, waiting for a
//! disk) runs. When the holder never lets go, every other task that touches
//! the filesystem queues behind it and the desktop goes quiet with nothing in
//! the log. The multiplexer loop reports a contender that has waited more than
//! [`WAIT_NS`], with the syscall it is in (which names the path it was taking)
//! and how many tasks wait now; again every [`REPEAT_NS`] while it lasts.
//! The lock's own name and holder are not known: the relax strategy is not
//! told which lock it serves. The holder is a task parked in a filesystem or
//! block syscall (`SCHED:STUCK` names runnable ones).

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{linuxstate, relax, MAX_TASKS, TASKS};

/// A wait this long is worth a line.
const WAIT_NS: u64 = 500_000_000;
/// Time between two lines for one wait.
const REPEAT_NS: u64 = 5_000_000_000;
/// Lines one sample prints.
const ROWS_MAX: usize = 6;

/// Per slot, when the wait was last reported (monotonic ns; 0: not yet).
static PRINTED: Mutex<[u64; MAX_TASKS]> = Mutex::new([0; MAX_TASKS]);

#[derive(Clone, Copy)]
struct Row {
    slot: usize,
    tgid: usize,
    name: &'static str,
    comm: [u8; 16],
    call: Option<(bool, u64)>,
    waited_ns: u64,
}

/// Look for long waits; print them. Task context, interrupts off.
pub fn sample() {
    sample_at(crate::arch::clock::monotonic_ns());
}

fn sample_at(now: u64) {
    let mut rows = [None::<Row>; ROWS_MAX];
    let mut count = 0;
    let mut waiters = 0;
    without_interrupts(|| {
        let tasks = TASKS.lock();
        let mut printed = PRINTED.lock();
        for slot in 1..MAX_TASKS {
            let Some(waited) = relax::waiting_ns(slot, now) else {
                printed[slot] = 0;
                continue;
            };
            waiters += 1;
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            let due = waited >= WAIT_NS
                && (printed[slot] == 0 || now.saturating_sub(printed[slot]) >= REPEAT_NS);
            if !due || count == ROWS_MAX {
                continue;
            }
            printed[slot] = now;
            rows[count] = Some(Row {
                slot,
                tgid: match task.linux.tgid {
                    0 => slot,
                    leader => leader,
                },
                name: task.name,
                comm: linuxstate::comm_of(task).0,
                call: crate::arch::irqoff::task_syscall(slot).map(|(native, nr, _)| (native, nr)),
                waited_ns: waited,
            });
            count += 1;
        }
    });
    for row in rows.iter().flatten() {
        print(row, waiters);
    }
}

fn print(row: &Row, waiters: usize) {
    let len = row.comm.iter().position(|&b| b == 0).unwrap_or(16);
    let comm = core::str::from_utf8(&row.comm[..len]).unwrap_or("?");
    let (abi, nr) = match row.call {
        Some((true, nr)) => ("native", nr as i64),
        Some((false, nr)) => ("linux", nr as i64),
        None => ("-", -1),
    };
    crate::trace::line!(
        tracefmt::Channel::Lock,
        "WAIT thread={}/{} tgid={} abi={} nr={} waiters={} ms={} comm={}",
        row.slot,
        row.name,
        row.tgid,
        abi,
        nr,
        waiters,
        row.waited_ns / 1_000_000,
        comm
    );
}

/// Forget what was reported (the kernel suite).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    without_interrupts(|| *PRINTED.lock() = [0; MAX_TASKS]);
}

/// [`sample`] at a chosen time; returns the slots it reported (the suite).
#[cfg(lazyos_tests)]
pub fn sample_at_for_test(now: u64) -> alloc::vec::Vec<usize> {
    let before = without_interrupts(|| *PRINTED.lock());
    sample_at(now);
    let after = without_interrupts(|| *PRINTED.lock());
    (0..MAX_TASKS)
        .filter(|&slot| after[slot] != before[slot] && after[slot] != 0)
        .collect()
}
