//! The bounds of the waiter table and every way a place is given back.

use super::*;
use crate::ipc::registry::wait::{Limits, MAX_WAITERS_PER_TASK};

/// Tickets are bounded per task and in all, and every way out of a wait gives
/// the place back: a drop, the teardown hook, and a task that died parked.
pub(super) fn wait_limits_and_release() -> Result<(), String> {
    fresh()?;
    // One wait per task.
    check!(MAX_WAITERS_PER_TASK == 1, "the per-task bound changed");
    let first = Ticket::take(task::KERNEL_TASK, "os.example.a").map_err(reason)?;
    check!(
        Ticket::take(task::KERNEL_TASK, "os.example.b").err()
            == Some(RegistryError::TooManyWaiters),
        "a task took a second ticket"
    );
    check!(
        wait_until("os.example.b", 3) == Err(RegistryError::TooManyWaiters),
        "a task waited for a second name"
    );
    drop(first);
    drop(Ticket::take(task::KERNEL_TASK, "os.example.b").map_err(reason)?);
    // The global bound, with three live tasks and a cap of three.
    let three = Limits {
        global: 3,
        per_process: 16,
        reserve: 0,
    };
    let tasks: Vec<usize> = (0..3)
        .map(|_| task::spawn_fork().map_err(to_string))
        .collect::<Result<_, _>>()?;
    let held: Vec<Ticket> = tasks
        .iter()
        .map(|&slot| {
            Ticket::take_with(slot, slot as u64, true, "os.example.g", &three).map_err(reason)
        })
        .collect::<Result<_, _>>()?;
    check!(
        Ticket::take_with(task::KERNEL_TASK, 0, true, "os.example.g", &three).err()
            == Some(RegistryError::TooManyWaiters),
        "the global bound let a fourth waiter in"
    );
    check!(
        registry::stats().waiters == 3,
        "the table holds the wrong count"
    );
    // The teardown hook, then a task that dies without it.
    wait::forget_waiter(tasks[0]);
    check!(
        registry::stats().waiters == 2,
        "forget_waiter kept the entry"
    );
    task::harness::finish(tasks[1], 0);
    check!(
        registry::stats().waiters == 1,
        "a dead task's ticket survived the prune"
    );
    drop(held);
    // The per-process bound: three tasks of one (made-up) process, two allowed.
    let two = Limits {
        global: 64,
        per_process: 2,
        reserve: 0,
    };
    let a = Ticket::take_with(tasks[0], 99, true, "os.example.p", &two).map_err(reason)?;
    let b = Ticket::take_with(tasks[2], 99, true, "os.example.p", &two).map_err(reason)?;
    check!(
        Ticket::take_with(task::KERNEL_TASK, 99, true, "os.example.p", &two).err()
            == Some(RegistryError::TooManyWaiters),
        "one process took a third place"
    );
    check!(
        Ticket::take_with(task::KERNEL_TASK, 98, true, "os.example.p", &two).is_ok(),
        "another process was refused by the first one's bound"
    );
    drop((a, b));
    // The reserve: a non-platform task stops short of it, a platform one does not.
    let reserved = Limits {
        global: 4,
        per_process: 16,
        reserve: 2,
    };
    let first = Ticket::take_with(tasks[0], 1, false, "os.example.r", &reserved).map_err(reason)?;
    let second =
        Ticket::take_with(tasks[2], 2, false, "os.example.r", &reserved).map_err(reason)?;
    check!(
        Ticket::take_with(task::KERNEL_TASK, 3, false, "os.example.r", &reserved).err()
            == Some(RegistryError::TooManyWaiters),
        "a labelled task took a reserved place"
    );
    let system =
        Ticket::take_with(task::KERNEL_TASK, 3, true, "os.example.r", &reserved).map_err(reason)?;
    drop((first, second, system));
    for slot in [tasks[0], tasks[2]] {
        task::harness::finish(slot, 0);
    }
    nothing_waits()?;
    registry::reset();
    Ok(())
}

/// The per-process bound keys on the address space, not on the lowest slot
/// that shares it: when the leader thread exits mid-wait the process keeps its
/// cap (the old key, the thread-group id, moved to another slot and let it
/// escape), and an unrelated process is not charged for it.
pub(super) fn wait_process_cap_survives_leader_exit() -> Result<(), String> {
    fresh()?;
    let share = crate::ipc::registry::wait::MAX_WAITERS_PER_PROCESS;
    let thread_in = |parent: usize| -> Result<usize, String> {
        task::harness::switch_current(parent);
        let thread = task::spawn_thread("waiter", process::USER_STACK_TOP, 0, 0);
        task::harness::switch_current(task::KERNEL_TASK);
        thread.map_err(String::from)
    };
    let leader = task::spawn_fork().map_err(to_string)?;
    let mut threads = vec![leader];
    for _ in 1..share {
        threads.push(thread_in(leader)?);
    }
    let mut tickets = Vec::new();
    for &slot in &threads {
        tickets.push(Ticket::take(slot, "os.example.q").map_err(reason)?);
    }
    let extra = thread_in(leader)?;
    check!(
        Ticket::take(extra, "os.example.q").err() == Some(RegistryError::TooManyWaiters),
        "the process took more than its {share} places"
    );
    // The leader (lowest slot) exits; its wait ends with it.
    drop(tickets.remove(0));
    task::harness::finish(leader, 0);
    // {share} - 1 are held: one more fits, the next does not.
    let again = Ticket::take(extra, "os.example.q").map_err(reason)?;
    let more = thread_in(threads[1])?;
    check!(
        Ticket::take(more, "os.example.q").err() == Some(RegistryError::TooManyWaiters),
        "the cap went with the leader"
    );
    // An unrelated process is not charged.
    let other = task::spawn_fork().map_err(to_string)?;
    let theirs = Ticket::take(other, "os.example.q").map_err(reason)?;
    drop((again, theirs, tickets));
    task::harness::reset();
    nothing_waits()
}
