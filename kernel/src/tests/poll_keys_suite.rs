//! Keyed `poll`/`select` wakeups (docs/performance-plan.md P6.5): a pipe or
//! pseudo-terminal event wakes only the poll waiters whose last scan looked
//! at that object; a waiter that met an unkeyed descriptor, or did not record
//! an interest at all, is woken by everything.

use super::*;
use crate::task::pollwait;
use crate::task::wait::POLL;
use crate::task::TaskState;

fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
}

fn blocked(slot: usize) -> bool {
    matches!(task::harness::state(slot), Some(TaskState::Blocked { .. }))
}

/// Make `slot` a poll waiter whose scan recorded `keys` (each entry one
/// descriptor's keys), parked on the poll queue.
fn park_with(slot: usize, keys: &[Option<[u64; 2]>]) {
    task::harness::switch_current(slot);
    task::poll_scan_begin();
    for &key in keys {
        pollwait::note_for_test(key);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    POLL.park_ns(slot, None);
}

/// The rules, one waiter at a time.
pub fn keyed_rules() -> Result<(), String> {
    fresh();
    let waiter = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let (a, b) = (0x1000u64, 0x2000u64);

    park_with(waiter, &[Some([a, 0])]);
    task::notify_poll_key(b);
    check!(
        blocked(waiter),
        "an event on another object woke the waiter"
    );
    task::notify_poll_key(a);
    check!(
        !blocked(waiter),
        "an event on its own object did not wake it"
    );
    POLL.forget(waiter);

    // A socket's two pipes: either wakes it.
    park_with(waiter, &[Some([a, b])]);
    task::notify_poll_key(b);
    check!(
        !blocked(waiter),
        "the second key of a socket did not wake it"
    );
    POLL.forget(waiter);

    // An unkeyed descriptor in the scan: everything wakes it.
    park_with(waiter, &[Some([a, 0]), None]);
    task::notify_poll_key(b);
    check!(
        !blocked(waiter),
        "an all-interest waiter slept through an event"
    );
    POLL.forget(waiter);

    // More keys than recorded: falls back to all.
    let many: Vec<Option<[u64; 2]>> = (1..=17u64).map(|key| Some([key * 0x100, 0])).collect();
    park_with(waiter, &many);
    task::notify_poll_key(0xdead_0000);
    check!(
        !blocked(waiter),
        "an overflowing interest did not fall back to all"
    );
    POLL.forget(waiter);

    // A scan that saw nothing yet wants nothing, but the unkeyed
    // notification (eventfds, sockets, the terminal) still wakes it.
    park_with(waiter, &[]);
    check!(
        !pollwait::wants(waiter, b),
        "an empty scan wants an event it never looked at"
    );
    task::notify_poll();
    check!(!blocked(waiter), "an unkeyed notification did not wake it");
    POLL.forget(waiter);

    task::harness::finish(waiter, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    Ok(())
}

/// 64 waiters, each watching its own object, and 200 000 random events: an
/// event wakes exactly the waiter of its object (when parked) and nobody
/// else, and the queue holds exactly the parked waiters throughout.
pub fn keyed_soak() -> Result<(), String> {
    fresh();
    let mut waiters = Vec::new();
    for _ in 0..64 {
        waiters.push(task::spawn_fork().map_err(|error| format!("spawn: {error}"))?);
    }
    let key = |index: usize| 0x10_0000 + index as u64 * 64;
    for (index, &slot) in waiters.iter().enumerate() {
        park_with(slot, &[Some([key(index), 0])]);
    }
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    for round in 0..200_000u32 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let index = (seed % 64) as usize;
        let slot = waiters[index];
        let was_parked = blocked(slot);
        let parked_before = POLL.len();
        task::notify_poll_key(key(index));
        let woken = parked_before - POLL.len();
        check!(
            woken == usize::from(was_parked) && !blocked(slot),
            "round {round}: event {index} woke {woken} (its waiter was parked: {was_parked})"
        );
        // Re-park a waiter that is awake.
        let again = ((seed >> 20) % 64) as usize;
        if !blocked(waiters[again]) {
            park_with(waiters[again], &[Some([key(again), 0])]);
        }
    }
    for &slot in &waiters {
        POLL.forget(slot);
        task::harness::finish(slot, 0);
    }
    while task::reap_child().is_some() {}
    check!(
        POLL.is_empty(),
        "{} entries left on the poll queue",
        POLL.len()
    );
    task::harness::reset();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("poll_keyed_rules", keyed_rules),
    ("poll_keyed_soak", keyed_soak),
];
