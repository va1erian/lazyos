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
//!   in one syscall at a time, so a second is a bug), [`MAX_WAITERS_PER_PROCESS`]
//!   per address space (threads are slots, so one process could otherwise
//!   take the whole table), and [`MAX_WAITERS`] in all, of which
//!   [`SYSTEM_RESERVE`] are kept for platform tasks (no label, no session). More is
//!   [`Error::TooManyWaiters`] (`EAGAIN`); the client runtimes then fall back
//!   to a bounded plain-`Resolve` retry, so an overloaded table degrades
//!   a connect, it does not fail it.
//! * **A signal ends the wait.** Any `Interrupted` wake that finds no name
//!   returns [`Error::Interrupted`] (`EINTR`) so the syscall reaches its
//!   return, where the signal frame is laid out; the client re-issues the
//!   wait with the same absolute deadline. Looping instead would spin with
//!   interrupts off when `wait` refuses to park (a deferred frame).
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
/// Waits one process (address space) may hold. A program has a handful of
/// threads connecting at once (a browser's workers, an app's UI and I/O
/// threads); 16 covers that and leaves the 512 to many programs.
pub const MAX_WAITERS_PER_PROCESS: usize = 16;
/// Of [`MAX_WAITERS`], the places only platform tasks (no label, no login
/// session: the services `init` starts before login) may take, so apps and
/// the user's programs cannot starve the services' connects. A service that
/// starts after login runs in a session and shares the open part.
pub const SYSTEM_RESERVE: usize = 128;

/// The three bounds of [`Ticket::take_with`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Waiters in all.
    pub global: usize,
    /// Waiters of one process.
    pub per_process: usize,
    /// Places of `global` only a platform task may take.
    pub reserve: usize,
}

/// The kernel's bounds.
pub const LIMITS: Limits = Limits {
    global: MAX_WAITERS,
    per_process: MAX_WAITERS_PER_PROCESS,
    reserve: SYSTEM_RESERVE,
};

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
    /// Its address space root, for the per-process bound. Not the thread-group
    /// id: that is the lowest slot sharing the space and changes when that
    /// thread exits, which would free the cap and charge the old value to an
    /// unrelated process.
    process: u64,
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
        let process = task::pml4_of(slot).unwrap_or(slot as u64);
        let cred = crate::ipc::credentials::of(slot);
        // Platform: no label and no login session (`0` before login is how
        // the kernel marks a system service). A user's own programs run in a
        // session, so they cannot fill the reserve.
        let platform = cred.label_id == 0 && cred.session == 0;
        Ticket::take_with(slot, process, platform, name, &LIMITS)
    }

    /// [`Ticket::take`] with the task's process and class given and the
    /// bounds as a parameter, so a test can reach them with a handful of
    /// tasks instead of hundreds.
    pub fn take_with(
        slot: usize,
        process: u64,
        platform: bool,
        name: &str,
        limits: &Limits,
    ) -> Result<Ticket, Error> {
        validate_name(name)?;
        let now = task::ticks();
        let live = super::live_slots();
        let mut registry = REGISTRY.lock();
        // Dead tasks' tickets first, so they cannot crowd a live one out.
        super::prune_locked(&mut registry, &live, now);
        let ceiling = if platform {
            limits.global
        } else {
            limits.global.saturating_sub(limits.reserve)
        };
        let held =
            |keep: &dyn Fn(&Waiter) -> bool| registry.waiters.iter().filter(|w| keep(w)).count();
        if registry.waiters.len() >= ceiling
            || held(&|w| w.slot == slot) >= MAX_WAITERS_PER_TASK
            || held(&|w| w.process == process) >= limits.per_process
        {
            return Err(Error::TooManyWaiters);
        }
        let id = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        registry.waiters.push(Waiter {
            id,
            slot,
            process,
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

/// Park `slot` on the wait queue as `wait_for` would, without running its
/// loop (suite hook: a task the suite cannot run, really `Blocked`).
#[cfg(lazyos_tests)]
pub fn park_for_test(slot: usize, deadline: Option<u64>) {
    NAME_WAIT.park(slot, deadline);
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
    wait_for_unless(target_slot, name, deadline, allowed, || {
        task::signal::must_unwind(me)
    })
}

/// [`wait_for`] with the "this task is being killed" test as a parameter, so
/// the suite (whose waiter is always the kernel task, which is never killed)
/// can drive the `Canceled` exit.
pub fn wait_for_unless(
    target_slot: usize,
    name: &str,
    deadline: Option<u64>,
    allowed: impl Fn() -> bool,
    must_unwind: impl Fn() -> bool,
) -> Result<u64, Error> {
    let me = task::current();
    // Before the first look, so a registration in between cannot be missed.
    let _ticket = Ticket::take(me, name)?;
    let mut interrupted = false;
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
        if interrupted {
            // Looked once more above; no name, so end the syscall and let the
            // signal (or the kill) take effect.
            return Err(Error::Interrupted);
        }
        if deadline.is_some_and(|limit| limit <= task::ticks()) {
            return Err(Error::TimedOut);
        }
        let reason = NAME_WAIT.wait(me, deadline);
        if reason == WakeReason::Interrupted {
            // A task being killed unwinds; any other signal returns `EINTR`
            // after one more look (`wait` does not park again while a handler
            // frame is deferred, so looping would spin).
            if must_unwind() {
                return Err(Error::Canceled);
            }
            interrupted = true;
        }
    }
}
