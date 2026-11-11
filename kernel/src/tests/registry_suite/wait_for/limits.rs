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
        .map(|&slot| Ticket::take_with(slot, slot, false, "os.example.g", &three).map_err(reason))
        .collect::<Result<_, _>>()?;
    check!(
        Ticket::take_with(task::KERNEL_TASK, 0, false, "os.example.g", &three).err()
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
    let a = Ticket::take_with(tasks[0], 99, false, "os.example.p", &two).map_err(reason)?;
    let b = Ticket::take_with(tasks[2], 99, false, "os.example.p", &two).map_err(reason)?;
    check!(
        Ticket::take_with(task::KERNEL_TASK, 99, false, "os.example.p", &two).err()
            == Some(RegistryError::TooManyWaiters),
        "one process took a third place"
    );
    check!(
        Ticket::take_with(task::KERNEL_TASK, 98, false, "os.example.p", &two).is_ok(),
        "another process was refused by the first one's bound"
    );
    drop((a, b));
    // The reserve: a labelled task stops short of it, an unlabelled one does not.
    let reserved = Limits {
        global: 4,
        per_process: 16,
        reserve: 2,
    };
    let first = Ticket::take_with(tasks[0], 1, true, "os.example.r", &reserved).map_err(reason)?;
    let second = Ticket::take_with(tasks[2], 2, true, "os.example.r", &reserved).map_err(reason)?;
    check!(
        Ticket::take_with(task::KERNEL_TASK, 3, true, "os.example.r", &reserved).err()
            == Some(RegistryError::TooManyWaiters),
        "a labelled task took a reserved place"
    );
    let system = Ticket::take_with(task::KERNEL_TASK, 3, false, "os.example.r", &reserved)
        .map_err(reason)?;
    drop((first, second, system));
    for slot in [tasks[0], tasks[2]] {
        task::harness::finish(slot, 0);
    }
    nothing_waits()?;
    registry::reset();
    Ok(())
}
