//! The kill watchdog (issue #858): name a process that was sent `SIGKILL`
//! and has not finished a second later.
//!
//! `SIGKILL` ends a thread where it stands only when its saved frame is a
//! user frame ([`kill_process`](super::kill_process)); a thread that is
//! runnable inside the kernel dies when its syscall returns. A kernel loop
//! that keeps it there makes the process unkillable, and `kill(pid, 0)`
//! keeps answering 0, so nothing says which loop. This line does:
//!
//! ```text
//! SIGNAL:KILL:SLOW tgid=41 thread=50 state=Runnable krip=0xffff8000... comm=NetworkProcess
//! ```
//!
//! `krip` is where the thread entered the scheduler last (symbolize it with
//! `addr2line -e <kernel> <krip - 0xffff800000000000>`), one line per thread
//! still alive, at most [`MAX_LINES`] per process, once per kill. It runs in
//! task context from the multiplexer loop, like the CPU guard's report,
//! because it takes the serial lock.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// How long a killed process may take to finish before it is named.
pub const SLOW_NS: u64 = 1_000_000_000;
/// How often [`service`] looks (it runs once per multiplexer iteration).
const CHECK_NS: u64 = 100_000_000;
/// Threads named per process.
const MAX_LINES: usize = 8;

/// The earliest time the next look is due.
static NEXT_NS: AtomicU64 = AtomicU64::new(0);

/// Look for killed processes that are slow to finish, at most every
/// [`CHECK_NS`]. Task context only (it prints).
pub fn service() {
    let now = crate::arch::clock::monotonic_ns();
    if now < NEXT_NS.load(Ordering::Relaxed) {
        return;
    }
    NEXT_NS.store(now + CHECK_NS, Ordering::Relaxed);
    check(now);
}

/// One look at time `now`: print the line for every process whose kill began
/// [`SLOW_NS`] ago and whose threads are still alive. Returns how many lines
/// were written. A process is reported once, however long it takes.
pub fn check(now: u64) -> usize {
    let due: Vec<u64> = peek(|procs, _| {
        procs
            .iter()
            .filter(|state| {
                state.kill_at_ns != 0
                    && !state.slow_reported
                    && now.saturating_sub(state.kill_at_ns) >= SLOW_NS
            })
            .map(|state| state.pml4)
            .collect()
    });
    let mut printed = 0;
    for pml4 in due {
        printed += report(pml4);
    }
    printed
}

/// Name the live threads of process `pml4` and mark the process reported
/// (a process with no live thread has finished: nothing to say).
fn report(pml4: u64) -> usize {
    let mut rows: Vec<(usize, usize, TaskState, u64)> = Vec::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.pml4 != pml4 || task.state == TaskState::Done {
                continue;
            }
            let tgid = match task.linux.tgid {
                0 => slot,
                leader => leader,
            };
            rows.push((
                slot,
                tgid,
                task.state,
                frame_word(task.rsp, FRAME_RIP_INDEX),
            ));
        }
    }
    if rows.is_empty() {
        return 0;
    }
    with_signals(pml4, |state| state.slow_reported = true);
    let mut printed = 0;
    for (slot, tgid, state, krip) in rows.into_iter().take(MAX_LINES) {
        let (name, len) = crate::task::linuxstate::comm(slot);
        let comm = core::str::from_utf8(&name[..len]).unwrap_or("?");
        crate::trace::line!(
            tracefmt::Channel::Signal,
            "KILL:SLOW tgid={tgid} thread={slot} state={state:?} krip={krip:#x} comm={comm}"
        );
        printed += 1;
    }
    printed
}
