//! A task that is being killed knows it (`signal::fatal_pending`) and a kill
//! that is slow to finish is named (`SIGNAL:KILL:SLOW`, issue #858).
//!
//! The harness has no second CPU to spin in a kernel loop, so these tests
//! stand a kernel-frame thread in for the loop that holds the process (the
//! saved frame is what `kill_process` decides on), and drive the watchdog
//! with the clock it is given.

use super::*;
use crate::task::wait::WaitQueue;
use alloc::string::String;

/// A native child of the kernel task whose saved frame looks like a thread
/// that yielded inside a syscall. Returns the slot and the user `CS` its
/// frame had.
fn kernel_child() -> Result<(usize, u64), String> {
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    task::harness::set_kind(slot, task::Kind::Native);
    let cs = task::harness::set_kernel_frame(slot).ok_or("the child has no frame")?;
    Ok((slot, cs))
}

/// Lines the trace sink would have written.
static LINES: spin::Mutex<Vec<String>> = spin::Mutex::new(Vec::new());

fn capture(line: &[u8]) {
    LINES
        .lock()
        .push(String::from(core::str::from_utf8(line).unwrap_or("?")));
}

/// The captured lines that are kill-watchdog reports.
fn slow_lines() -> Vec<String> {
    LINES
        .lock()
        .iter()
        .filter(|line| line.starts_with("SIGNAL:KILL:SLOW"))
        .cloned()
        .collect()
}

/// `fatal_pending` is true for exactly the tasks of a process that was sent
/// `SIGKILL`, and for no other process nor the kernel task.
pub fn fatal_pending_follows_the_kill() -> Result<(), String> {
    fresh()?;
    let (victim, cs) = kernel_child()?;
    let (bystander, _) = kernel_child()?;
    for slot in [victim, bystander, task::KERNEL_TASK] {
        task::harness::switch_current(slot);
        check!(
            !signal::fatal_pending(),
            "slot {slot} is pending before a kill"
        );
    }
    task::harness::switch_current(task::KERNEL_TASK);
    send(victim, signal::SIGKILL)?;
    for (slot, want) in [
        (victim, true),
        (bystander, false),
        (task::KERNEL_TASK, false),
    ] {
        task::harness::switch_current(slot);
        check!(
            signal::fatal_pending() == want,
            "slot {slot}: fatal_pending should be {want}"
        );
    }
    task::harness::switch_current(task::KERNEL_TASK);
    // A thread of the victim's process sees it too.
    task::harness::switch_current(victim);
    let thread = task::spawn_thread("killwatch-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    task::harness::switch_current(thread);
    check!(
        signal::fatal_pending(),
        "a new thread of a killed process is not pending"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::set_frame_cs(victim, cs);
    check!(
        task::harness::resume_delivery(victim),
        "the killed task was not ended"
    );
    task::harness::finish(bystander, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The watchdog names a killed process that is still alive a second later
/// (once), says nothing earlier, and says nothing of one that finished.
pub fn kill_slow_line_names_the_stuck_thread() -> Result<(), String> {
    use crate::task::signal::killwatch;
    fresh()?;
    crate::trace::reset_for_test();
    LINES.lock().clear();
    crate::trace::capture::set(capture);
    let result = (|| -> Result<(), String> {
        let now = crate::arch::clock::monotonic_ns();
        let (stuck, cs) = kernel_child()?;
        let (quick, quick_cs) = kernel_child()?;
        send(stuck, signal::SIGKILL)?;
        send(quick, signal::SIGKILL)?;
        // `quick` unwinds at once.
        task::harness::set_frame_cs(quick, quick_cs);
        check!(task::harness::resume_delivery(quick), "quick was not ended");
        check!(
            killwatch::check(now + killwatch::SLOW_NS / 2) == 0,
            "the watchdog spoke before the second was up"
        );
        let printed = killwatch::check(now + 2 * killwatch::SLOW_NS);
        check!(
            printed == 1,
            "{printed} lines, expected one (the stuck thread)"
        );
        if crate::trace::ENABLED {
            let lines = slow_lines();
            check!(
                lines.len() == 1,
                "{} SLOW lines captured: {lines:?}",
                lines.len()
            );
            let line = &lines[0];
            check!(
                line.contains(&format!(
                    "tgid={stuck} thread={stuck} state=Runnable krip=0x"
                )),
                "the line does not name the thread: {line}"
            );
        }
        check!(
            killwatch::check(now + 3 * killwatch::SLOW_NS) == 0,
            "the same kill was reported twice"
        );
        // It finally leaves the kernel.
        task::harness::set_frame_cs(stuck, cs);
        check!(task::harness::resume_delivery(stuck), "stuck was not ended");
        while task::reap_child().is_some() {}
        check!(
            killwatch::check(now + 4 * killwatch::SLOW_NS) == 0,
            "a finished process was reported"
        );
        Ok(())
    })();
    crate::trace::capture::clear();
    crate::trace::reset_for_test();
    task::harness::reset();
    signal::harness::reset();
    result
}

/// Soak: 1000 kills of processes in every state a killed process can be in,
/// each reaped as `128 + SIGKILL` with nothing left behind: a thread in user
/// mode, a thread parked in the kernel, one runnable in the kernel (it learns
/// of the kill through `fatal_pending`), a leader with threads, and a leader
/// that exited before the kill.
pub fn soak_kill_every_state() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let registry = signal::harness::registry_len();
    let before = crate::mem::frame_stats().live();
    for round in 0..1000u32 {
        let fail = |what: &str| format!("round {round}: {what}");
        let (leader, cs) = kernel_child()?;
        match round % 5 {
            // Runnable in the kernel: it must see the kill, then unwinds.
            0 => {
                send(leader, signal::SIGKILL)?;
                task::harness::switch_current(leader);
                let pending = signal::fatal_pending();
                task::harness::switch_current(task::KERNEL_TASK);
                check!(
                    pending,
                    "{}",
                    fail("a runnable kernel thread missed the kill")
                );
            }
            // Parked in the kernel: woken, and cannot park again.
            1 => {
                queue.park(leader, None);
                send(leader, signal::SIGKILL)?;
                check!(
                    task::harness::take_wake_reason(leader) == Some(WakeReason::Interrupted),
                    "{}",
                    fail("the parked thread was not woken")
                );
                check!(
                    queue.wait(leader, None) == WakeReason::Interrupted,
                    "{}",
                    fail("it parked again")
                );
                queue.forget(leader);
            }
            // In user mode: ended at the send.
            2 => {
                task::harness::set_frame_cs(leader, cs);
                send(leader, signal::SIGKILL)?;
                check!(
                    task::harness::state(leader) == Some(TaskState::Done),
                    "{}",
                    fail("a user-mode thread survived the send")
                );
            }
            // A leader with threads: the threads (user mode) end at the send,
            // the leader (kernel) when it unwinds.
            3 => {
                task::harness::switch_current(leader);
                let a = task::spawn_thread("killwatch-a", process::USER_STACK_TOP, 0, 0);
                let b = task::spawn_thread("killwatch-b", process::USER_STACK_TOP, 0, 0);
                task::harness::switch_current(task::KERNEL_TASK);
                let (a, b) = (a.map_err(String::from)?, b.map_err(String::from)?);
                send(leader, signal::SIGKILL)?;
                for thread in [a, b] {
                    check!(
                        task::harness::state(thread) == Some(TaskState::Done),
                        "{}",
                        fail("a user-mode thread of the leader survived")
                    );
                }
            }
            // The leader exited before the kill; a thread still runs.
            _ => {
                task::harness::set_frame_cs(leader, cs);
                task::harness::switch_current(leader);
                let thread = task::spawn_thread("killwatch-c", process::USER_STACK_TOP, 0, 0)
                    .map_err(String::from)?;
                task::harness::switch_current(task::KERNEL_TASK);
                task::harness::finish(leader, 0);
                send(leader, signal::SIGKILL)?;
                check!(
                    task::harness::state(thread) == Some(TaskState::Done),
                    "{}",
                    fail("the thread of an exited leader survived")
                );
                check!(
                    task::reap_child().map(|(slot, _)| slot) == Some(leader),
                    "{}",
                    fail("the exited leader was not reaped")
                );
            }
        }
        if round % 5 != 4 {
            task::harness::set_frame_cs(leader, cs);
            if task::harness::state(leader) != Some(TaskState::Done) {
                check!(
                    task::harness::resume_delivery(leader),
                    "{}",
                    fail("the unwound thread was not ended")
                );
            }
            check!(
                task::reap_child() == Some((leader, 128 + signal::SIGKILL as u64)),
                "{}",
                fail("the leader did not exit with 128 + SIGKILL")
            );
        }
        // The harness lets the kernel task (parent 0) collect thread corpses
        // too; a process with no threads leaves none.
        let mut extra = 0;
        while task::reap_child().is_some() {
            extra += 1;
        }
        check!(
            extra
                == if round % 5 >= 3 {
                    2 - (round % 5 - 3)
                } else {
                    0
                },
            "{}",
            fail(&format!("{extra} extra corpses"))
        );
        // Hand the Done threads back (the scheduler does it on a tick).
        task::harness::simulate_tick();
        task::harness::switch_current(task::KERNEL_TASK);
        task::reclaim_pending();
    }
    check!(
        queue.is_empty(),
        "{} wait-queue entries leaked",
        queue.len()
    );
    let left = signal::harness::registry_len();
    check!(
        left <= registry,
        "signal-registry entries leaked: {registry} -> {left}"
    );
    task::harness::reset();
    signal::harness::reset();
    let after = crate::mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    crate::trace::test!("task_signal_soak_kill_every_state:INFO:1000 kill/unwind/reap cycles");
    Ok(())
}
