//! `WaitFor` with many tasks really `Blocked` on the wait queue.

use super::*;
use crate::task::WakeReason;

/// Tasks waiting for the name that gets registered.
const HERD: usize = 64;
/// Tasks waiting for another name: they must never wake.
const BYSTANDERS: usize = 8;
const ROUNDS: usize = 60;

/// Seventy-two forked tasks parked on the wait queue, 64 for one name and 8
/// for another, half of the 64 with a deadline a tick or so away. One
/// `Register` wakes exactly the name's waiters (`Woken`; or `TimedOut` for a
/// deadline that came first), none of the bystanders; in alternate rounds the
/// deadlines are forced past first, so the timed-out waiters are still in the
/// table when the registration scans it. After every round all tickets and
/// queue entries are given back, and after the last nothing is left.
pub(super) fn parked_herd_soak() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    let entry = handles::get(callable).map_err(friendly)?;
    let tasks: Vec<usize> = (0..HERD + BYSTANDERS)
        .map(|_| task::spawn_fork().map_err(to_string))
        .collect::<Result<_, _>>()?;
    let (herd, bystanders) = tasks.split_at(HERD);
    for round in 0..ROUNDS {
        let mut tickets = Vec::new();
        let now = task::ticks();
        for (index, &slot) in herd.iter().enumerate() {
            tickets.push(Ticket::take(slot, NAMES[0]).map_err(reason)?);
            // Odd tasks have a deadline a tick or two away.
            wait::park_for_test(slot, (index % 2 == 1).then_some(now + 2));
        }
        for &slot in bystanders {
            tickets.push(Ticket::take(slot, NAMES[1]).map_err(reason)?);
            wait::park_for_test(slot, None);
        }
        check!(
            wait::queue_len() == tasks.len() && wait::waiting_slots(NAMES[0]).len() == HERD,
            "round {round}: {} queued, {} waiting for the name",
            wait::queue_len(),
            wait::waiting_slots(NAMES[0]).len()
        );
        let forced = round % 2 == 1;
        if forced {
            task::harness::expire_deadlines(now + 10);
        }
        registry::register(
            task::KERNEL_TASK,
            NAMES[0],
            entry.kind,
            entry.rights,
            entry.object_id,
            &[1],
            0,
        )
        .map_err(reason)?;
        let (mut woken, mut timed_out) = (0, 0);
        for &slot in herd {
            match task::harness::take_wake_reason(slot) {
                Some(WakeReason::Woken) => woken += 1,
                Some(WakeReason::TimedOut) => timed_out += 1,
                other => return Err(format!("round {round}: waiter {slot} got {other:?}")),
            }
        }
        check!(
            woken >= HERD / 2 && woken + timed_out == HERD,
            "round {round}: {woken} woken, {timed_out} timed out"
        );
        check!(
            !forced || timed_out == HERD / 2,
            "round {round}: forced deadlines timed out {timed_out}"
        );
        for &slot in bystanders {
            check!(
                task::harness::take_wake_reason(slot).is_none(),
                "round {round}: a waiter for another name woke"
            );
        }
        // What each waiter's own loop does on the way out.
        for &slot in &tasks {
            wait::forget_waiter(slot);
            task::wake_task(slot);
            let _ = task::harness::take_wake_reason(slot);
        }
        drop(tickets);
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, NAMES[0]).map_err(reason)?;
        nothing_waits()?;
    }
    for slot in tasks {
        task::harness::finish(slot, 0);
    }
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}
