//! `execve`'s de-thread step: before the new image runs, every other thread
//! of the process has ended, as on Linux (`de_thread`). Without it the
//! siblings went on running the old program in the old address space
//! (`cargo run` left one behind every time).

use super::signal::{self, ExecRace};
use super::{current, wait, TASKS};

/// How long a wait for the siblings sleeps before it looks again. Every
/// death wakes [`wait::CHILD_EXIT`]; this only bounds a wake missed because
/// another CPU finished the last sibling between the count and the park.
const RECHECK_NS: u64 = 10_000_000;

/// End every other thread of the current task's process and wait until they
/// have finished. The old address space stays as it is (the exec'ing thread
/// still runs in it). `Err` when the process is being ended as a whole, or
/// another thread's `execve` won: the caller fails its `execve` and dies at
/// its syscall return. Call with interrupts off (syscall context).
pub fn dethread() -> Result<(), ExecRace> {
    let me = current();
    let Some(pml4) = TASKS.lock()[me].as_ref().map(|task| task.pml4) else {
        return Ok(());
    };
    // The common exec: a single-threaded process has nothing to end.
    if signal::exec_siblings_left(pml4, me) == 0 {
        return Ok(());
    }
    signal::begin_exec(pml4, me)?;
    loop {
        if signal::exec_siblings_left(pml4, me) == 0 {
            return Ok(());
        }
        // A kill of the process (a group exit sets one too), or of this
        // thread (which `begin_exec` would have refused): its syscall return
        // ends it.
        if signal::kill_pending(pml4, me) {
            return Err(ExecRace::Dying);
        }
        let deadline = crate::arch::clock::monotonic_ns().saturating_add(RECHECK_NS);
        let _ = wait::CHILD_EXIT.wait_killable_ns(me, Some(deadline));
    }
}
