//! Waiting for a name to register (`WaitFor`, `docs/client-api-plan.md` 3.4).
//!
//! A program that starts before the service it needs used to poll `Resolve`
//! on a timer. [`wait_for`] parks the task instead: it takes a [`Ticket`] in
//! the registry's waiter table, looks the name up, and sleeps on one wait
//! queue until a `Register` of *that name* wakes it or its deadline passes.
//!
//! * **No lost wake-up.** The ticket is taken before the first look, and a
//!   registration scans the table under the same lock that publishes the
//!   entry, so a `Register` between the look and the park is seen either by
//!   the look or by the scan. The syscall keeps the kernel entry lock (and
//!   interrupts off) from the look to the park, exactly as `recv` does.
//! * **Only the name's waiters wake.** The table keeps each waiter's name;
//!   [`waiting_for`] copies out the slots to notify and [`wake`] notifies
//!   them after the registry lock is released (wait queue, then task table;
//!   never inside the registry's lock).
//! * **Nothing leaks.** The ticket is released by its `Drop` on every way
//!   out (found, deadline, cancel). A task that dies parked never runs it,
//!   so [`forget_waiter`] (the teardown hook next to the channel one) and the
//!   access-time prune of dead slots drop its entry.
//! * **Bounded.** One wait per task ([`MAX_WAITERS_PER_TASK`]; a task parks
//!   in one syscall at a time, so a second is a bug) and [`MAX_WAITERS`] in
//!   all: more is [`Error::TooManyWaiters`] (`EAGAIN`), never a silent poll.
//! * **No new authority.** The caller's name policy runs at the syscall edge
//!   before the wait, and `allowed` runs again each time the name is found,
//!   before any handle opens, so a waiter gets what a `Resolve` at that
//!   moment would give and nothing about names it may not resolve.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{lookup, open_entry, validate_name, Error, Registry, REGISTRY};
use crate::task::wait::WaitQueue;
use crate::task::{self, WaitKind, WakeReason};

/// Tasks that may wait for names at once, kernel-wide.
pub const MAX_WAITERS: usize = 512;
/// Names one task may wait for at once. A task parks in a single syscall, so
/// one is exact; the limit is what stops a leaked ticket from stacking up.
pub const MAX_WAITERS_PER_TASK: usize = 1;

/// Everything parked in [`wait_for`] sleeps here; [`wake`] aims at one slot.
static NAME_WAIT: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// Source of [`Waiter::id`]s.
static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);

/// One entry of the registry's waiter table.
pub(super) struct Waiter {
    /// Unique per ticket, so a recycled slot's new ticket is never mistaken
    /// for a dead task's.
    id: u64,
    /// The waiting task's slot.
    pub(super) slot: usize,
    /// The name it waits for.
    name: String,
}

/// A task's place in the waiter table; dropping it gives the place back.
#[must_use = "a ticket held for nothing keeps a waiter slot"]
pub struct Ticket {
    id: u64,
}

impl Ticket {
    /// Enter the table for `slot` waiting on `name`.
    pub fn take(slot: usize, name: &str) -> Result<Ticket, Error> {
        Ticket::take_within(slot, name, MAX_WAITERS)
    }

    /// [`Ticket::take`] with the global bound as a parameter, so a test can
    /// reach it with a handful of tasks instead of [`MAX_WAITERS`].
    pub fn take_within(slot: usize, name: &str, global_cap: usize) -> Result<Ticket, Error> {
        validate_name(name)?;
        let now = task::ticks();
        let live = super::live_slots();
        let mut registry = REGISTRY.lock();
        // Dead tasks' tickets first, so they cannot crowd a live one out.
        super::prune_locked(&mut registry, &live, now);
        if registry.waiters.len() >= global_cap
            || registry
                .waiters
                .iter()
                .filter(|waiter| waiter.slot == slot)
                .count()
                >= MAX_WAITERS_PER_TASK
        {
            return Err(Error::TooManyWaiters);
        }
        let id = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        registry.waiters.push(Waiter {
            id,
            slot,
            name: String::from(name),
        });
        Ok(Ticket { id })
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        REGISTRY
            .lock()
            .waiters
            .retain(|waiter| waiter.id != self.id);
    }
}

/// The slots waiting for `name`: what a registration of it must wake. Called
/// with the registry locked.
pub(super) fn waiting_for(registry: &Registry, name: &str) -> Vec<usize> {
    registry
        .waiters
        .iter()
        .filter(|waiter| waiter.name == name)
        .map(|waiter| waiter.slot)
        .collect()
}

/// Wake each of `slots` that is parked here. The registry lock must not be
/// held. A slot that already left (deadline, kill) is skipped by the queue.
pub(super) fn wake(slots: &[usize]) {
    for &slot in slots {
        NAME_WAIT.notify_task(slot);
    }
}

/// How many tickets are held, and the slots waiting for `name` (suite hooks).
#[cfg(lazyos_tests)]
pub fn waiting_slots(name: &str) -> Vec<usize> {
    waiting_for(&REGISTRY.lock(), name)
}

/// Tasks parked on the wait queue (suite hook: a leak is a length that stays up).
#[cfg(lazyos_tests)]
pub fn queue_len() -> usize {
    NAME_WAIT.len()
}

/// Drop every trace of `slot` as a waiter: the teardown hook for a task that
/// died parked (its wait loop never ran), so a reused slot starts clean.
pub fn forget_waiter(slot: usize) {
    REGISTRY.lock().waiters.retain(|waiter| waiter.slot != slot);
    NAME_WAIT.forget(slot);
}

/// Resolve `name` into `target_slot`'s table, waiting for it to register until
/// `deadline` (absolute PIT ticks; `None` waits for ever).
///
/// `allowed` is the caller's name policy, asked each time the name is found
/// and before a handle opens; a refusal ends the wait with [`Error::Denied`].
/// Must run in syscall context on the waiting task itself, with interrupts
/// off and no lock held.
pub fn wait_for(
    target_slot: usize,
    name: &str,
    deadline: Option<u64>,
    allowed: impl Fn() -> bool,
) -> Result<u64, Error> {
    let me = task::current();
    // Before the first look, so a registration in between cannot be missed.
    let _ticket = Ticket::take(me, name)?;
    loop {
        match lookup(name) {
            Ok(found) => {
                if !allowed() {
                    return Err(Error::Denied);
                }
                return open_entry(target_slot, found);
            }
            Err(Error::UnknownName) => {}
            Err(error) => return Err(error),
        }
        if deadline.is_some_and(|limit| limit <= task::ticks()) {
            return Err(Error::TimedOut);
        }
        let reason = NAME_WAIT.wait(me, deadline);
        // A fatal signal must reach the syscall return where the task ends;
        // parking again would keep it alive (`recv` does the same).
        if reason == WakeReason::Interrupted && task::signal::must_unwind(me) {
            return Err(Error::Canceled);
        }
    }
}
