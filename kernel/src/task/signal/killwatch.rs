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
/// Lines one look may write, over all processes; the rest wait for the next
/// look, so a mass kill cannot flood the serial port.
const MAX_PER_CHECK: usize = 16;
/// Stack words searched for return addresses, and addresses printed.
const STACK_WORDS: usize = 64;
const CALLS: usize = 6;
/// The kernel image sits at the bottom of the bootloader's mappings.
const IMAGE_START: u64 = 0xffff_8000_0000_0000;
const IMAGE_END: u64 = IMAGE_START + 0x0400_0000;

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
        if printed >= MAX_PER_CHECK {
            break;
        }
        printed += report(pml4);
    }
    printed
}

/// Words on the saved kernel stack at `rsp` that point into the kernel image:
/// the return addresses of the loop that holds the thread (`krip` alone is
/// the scheduler entry it yielded through). Stack garbage that happens to
/// look like an address is possible; they are leads, not a backtrace.
fn calls(rsp: u64) -> ([u64; CALLS], usize) {
    let mut found = [0u64; CALLS];
    let mut count = 0;
    // Same contract as `arch::nmi::write_stack`: `rsp` is a ring-0 stack
    // pointer the scheduler saved, whose page is mapped; a page-aligned one
    // may sit at the very top of its stack, so it is skipped.
    if !rsp.is_multiple_of(8) || rsp.is_multiple_of(4096) || rsp < IMAGE_START {
        return (found, 0);
    }
    let words = (((4096 - rsp % 4096) / 8) as usize).min(STACK_WORDS);
    for index in 0..words {
        // SAFETY: `index < words` keeps the read inside `rsp`'s page.
        let word = unsafe { core::ptr::read_volatile((rsp + index as u64 * 8) as *const u64) };
        if (IMAGE_START..IMAGE_END).contains(&word) && count < CALLS {
            found[count] = word;
            count += 1;
        }
    }
    (found, count)
}

/// Name the live threads of process `pml4` and mark the process reported
/// (a process with no live thread has finished: nothing to say).
fn report(pml4: u64) -> usize {
    let mut rows: Vec<(usize, usize, TaskState, u64, u64)> = Vec::new();
    // An `execve` waiting for its other threads to end is not stuck: they are.
    let execing = peek(|procs, _| {
        procs
            .iter()
            .find(|state| state.pml4 == pml4 && state.group_exit.is_none())
            .map_or(0, |state| state.exec_by)
    });
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.pml4 != pml4 || task.state == TaskState::Done || slot == execing {
                continue;
            }
            let tgid = match task.linux.tgid {
                0 => slot,
                leader => leader,
            };
            // A thread running on another CPU (S8.3c) has only a stale
            // frame: report no RIP and no stack for it.
            if crate::task::oncpu::elsewhere(slot) {
                rows.push((slot, tgid, task.state, 0, 0));
                continue;
            }
            // The kernel stack pointer of a thread inside the kernel.
            let in_kernel = !frame_is_user(task.rsp, FRAME_RIP_INDEX);
            let stack = if in_kernel && slot != current() {
                frame_word(task.rsp, FRAME_RIP_INDEX + 3)
            } else {
                0
            };
            rows.push((
                slot,
                tgid,
                task.state,
                frame_word(task.rsp, FRAME_RIP_INDEX),
                stack,
            ));
        }
    }
    // Reported, or finished with nothing left alive (a zombie not yet
    // reaped must not be looked at again and again).
    with_signals(pml4, |state| state.slow_reported = true);
    let mut printed = 0;
    for (slot, tgid, state, krip, stack) in rows.into_iter().take(MAX_LINES) {
        let (name, len) = crate::task::linuxstate::comm(slot);
        let comm = core::str::from_utf8(&name[..len]).unwrap_or("?");
        let (found, count) = calls(stack);
        let mut list = alloc::string::String::new();
        for address in &found[..count] {
            let _ = core::fmt::Write::write_fmt(&mut list, format_args!("{address:#x},"));
        }
        crate::trace::line!(
            tracefmt::Channel::Signal,
            "KILL:SLOW tgid={tgid} thread={slot} state={state:?} krip={krip:#x} calls={list} comm={comm}"
        );
        printed += 1;
    }
    printed
}

/// Test hook: processes [`check`] would look at at time `now`.
#[cfg(lazyos_tests)]
pub fn due(now: u64) -> usize {
    peek(|procs, _| {
        procs
            .iter()
            .filter(|state| {
                state.kill_at_ns != 0
                    && !state.slow_reported
                    && now.saturating_sub(state.kill_at_ns) >= SLOW_NS
            })
            .count()
    })
}
