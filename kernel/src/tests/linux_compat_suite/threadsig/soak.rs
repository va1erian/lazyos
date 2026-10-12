//! The per-thread signal soak: thousands of thread-directed and process-directed
//! signals at threads that spin, sleep in the kernel or block the signal.

use super::group::*;
use super::*;
use crate::task::signal;
use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

/// Soak: thousands of `tgkill`s and `kill`s at threads that spin, sleep or
/// block the signal. Each is taken exactly once, by the thread it was aimed at
/// (or by one thread that may take a process signal), nothing is left pending,
/// no sibling is woken for it, and the slots, frames and registry return to
/// the baseline.
pub fn threadsig_soak() -> Result<(), String> {
    const THREADS: usize = 6;
    const ROUNDS: usize = 3000;
    let baseline_frames = mem::frame_stats().live();
    let baseline_slots = task::process::process_list().len();
    let g = group(THREADS)?;
    let members = g.members();
    let mut handled = vec![0usize; members.len()];
    let mut expected = vec![0usize; members.len()];

    for round in 0..ROUNDS {
        let index = (round * 5 + round / 7) % members.len();
        let (target, bystander) = (members[index], members[(index + 1) % members.len()]);
        let mode = round % 5;
        let (target_queue, bystander_queue) = (
            WaitQueue::new(WaitKind::Sleep),
            WaitQueue::new(WaitKind::Sleep),
        );

        match mode {
            // Spinning in user mode, sleeping in a syscall, blocking the signal.
            0..=2 => {
                let sleeping_cs = if mode == 1 {
                    target_queue.park(target, None);
                    bystander_queue.park(bystander, None);
                    // The sleeper is inside a syscall: its saved frame is a kernel one.
                    task::harness::set_kernel_frame(target)
                } else {
                    None
                };
                if mode == 2 {
                    signal::set_blocked(target, kbit(USR1));
                }
                let sent = tgkill(&g, task::KERNEL_TASK, target, USR1);
                check!(sent == 0, "round {round}: tgkill returned {sent:#x}");
                check!(
                    signal::pending_thread(target) == kbit(USR1)
                        && g.thread_pending() == kbit(USR1),
                    "round {round}: queued on {:#x}",
                    g.thread_pending()
                );
                match sleeping_cs {
                    Some(cs) => {
                        // Woken for its own signal only, then back to user mode.
                        let woke = task::harness::take_wake_reason(target);
                        check!(
                            woke == Some(WakeReason::Interrupted),
                            "round {round}: the target slept through its signal: {woke:?}"
                        );
                        check!(is_blocked(bystander), "round {round}: the bystander woke");
                        task::harness::set_frame_cs(target, cs);
                        check!(
                            !g.resume(target),
                            "round {round}: resuming ended the thread"
                        );
                    }
                    None if mode == 2 => {
                        g.sweep();
                        check!(
                            g.entered_members().is_empty(),
                            "round {round}: blocked, yet ran"
                        );
                        signal::set_blocked(target, 0);
                        g.sweep();
                    }
                    None => g.sweep(),
                }
                expected[index] += 1;
            }
            // Process-directed; in mode 3 the leader blocks it.
            _ => {
                let leader_mask = if mode == 3 { kbit(USR1) } else { 0 };
                signal::set_blocked(g.leader, leader_mask);
                let pid = g.leader as u64;
                let sent = as_thread(task::KERNEL_TASK, KILL, &[pid, USR1 as u64]);
                check!(sent == 0, "round {round}: kill returned {sent:#x}");
                g.sweep();
            }
        }

        // Exactly one member took the signal; for a `tgkill`, the target.
        let entered = g.entered_members();
        check!(
            entered.len() == 1,
            "round {round} (mode {mode}): {entered:?} entered, expected one"
        );
        let taker = members.iter().position(|&s| s == entered[0]).unwrap_or(0);
        check!(
            mode >= 3 || taker == index,
            "round {round}: member {taker} took a signal aimed at {index}"
        );
        check!(
            mode != 3 || taker != 0,
            "round {round}: the blocking leader took it"
        );
        handled[taker] += 1;
        if mode >= 3 {
            expected[taker] += 1;
        }
        g.return_from_handler(entered[0], 0);
        signal::set_blocked(g.leader, 0);
        target_queue.notify_all();
        bystander_queue.notify_all();
        task::harness::take_wake_reason(bystander);
        check!(
            !is_blocked(target) && !is_blocked(bystander),
            "round {round}: a thread is still parked"
        );
        check!(
            g.thread_pending() == 0 && signal::pending(g.leader) == 0,
            "round {round}: pending left: {:#x} {:#x}",
            g.thread_pending(),
            signal::pending(g.leader)
        );
        if round % 1000 == 999 {
            crate::trace::test!(
                "compat_threadsig_soak:PROGRESS:round {}/{ROUNDS}",
                round + 1
            );
        }
    }

    check!(
        handled == expected,
        "handled {handled:?}, expected {expected:?}"
    );
    let total: usize = handled.iter().sum();
    check!(total == ROUNDS, "{total} handlers ran for {ROUNDS} signals");
    teardown(&g)?;
    check!(
        task::process::process_list().len() == baseline_slots,
        "task slots leaked: {} now, {baseline_slots} before",
        task::process::process_list().len()
    );
    check!(
        signal::harness::registry_len() == 0 && signal::harness::threads_in_use() == 0,
        "signal state leaked"
    );
    check!(
        mem::frame_stats().live() <= baseline_frames,
        "{} frames leaked",
        mem::frame_stats().live().saturating_sub(baseline_frames)
    );
    Ok(())
}
