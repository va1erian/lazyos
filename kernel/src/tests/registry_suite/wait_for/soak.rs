//! The soaks of `WaitFor`: sustained waits and waiter-table churn.

use super::*;

/// Many sequential waits on many names, each woken by a registration and
/// followed by a withdrawal, leave no ticket, queue entry or handle behind.
pub(super) fn wait_soak() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    let base = handles::count_for_task(task::KERNEL_TASK);
    let rounds = 240usize;
    for round in 0..rounds {
        let name = round % NAMES.len();
        // Every third round the name is there already; the rest wait.
        if round % 3 == 0 {
            no_later();
            publish(NAMES[name]);
        } else {
            later(PLAN_REGISTER, name, round as u64 % 3, callable)?;
        }
        let handle = wait_until(NAMES[name], 500).map_err(reason)?;
        no_later();
        channels::close_endpoint(handle).map_err(friendly)?;
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, NAMES[name]).map_err(reason)?;
    }
    check!(
        handles::count_for_task(task::KERNEL_TASK) == base,
        "{} handles left after the soak ({base} expected)",
        handles::count_for_task(task::KERNEL_TASK)
    );
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

/// The waiter table under churn: tickets of many tasks on many names, names
/// registered and withdrawn around them, tasks coming and going.
pub(super) fn wait_table_churn_soak() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    let entry = handles::get(callable).map_err(friendly)?;
    let tasks: Vec<usize> = (0..6)
        .map(|_| task::spawn_fork().map_err(to_string))
        .collect::<Result<_, _>>()?;
    for round in 0..2000usize {
        let mut held = Vec::new();
        for (index, &slot) in tasks.iter().enumerate() {
            let name = NAMES[(round + index) % NAMES.len()];
            held.push(Ticket::take(slot, name).map_err(reason)?);
        }
        let name = NAMES[round % NAMES.len()];
        registry::register(
            task::KERNEL_TASK,
            name,
            entry.kind,
            entry.rights,
            entry.object_id,
            &[1],
            0,
        )
        .map_err(reason)?;
        let woken = wait::waiting_slots(name);
        check!(
            !woken.is_empty() && woken.iter().all(|slot| tasks.contains(slot)),
            "round {round}: a registration saw waiters {woken:?}"
        );
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, name).map_err(reason)?;
        // Release in two different orders, so neither is the one that works.
        if round % 2 == 0 {
            held.reverse();
        }
        drop(held);
        check!(
            registry::stats().waiters == 0,
            "round {round}: {} tickets left",
            registry::stats().waiters
        );
    }
    for slot in tasks {
        task::harness::finish(slot, 0);
    }
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}
