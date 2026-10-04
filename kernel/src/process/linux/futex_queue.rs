//! The futex waiter table: who is parked on which futex word.
//!
//! A waiter is keyed by its address space (the PML4 of the waiting task) and
//! the word's user address, so two processes that happen to use the same
//! virtual address never wake each other. LazyOS has no memory shared between
//! processes, so a "shared" futex (one without `FUTEX_PRIVATE_FLAG`) has the
//! same key as a private one: only the caller's own address space can see it.
//!
//! Every waiter is one entry with a bitset (`FUTEX_WAIT_BITSET`); a plain wait
//! uses all ones. A wake removes the entry and marks the task runnable, a
//! requeue rewrites the entry's key, and a waiter that returns for any other
//! reason (timeout, signal) removes its own entry by slot. So an entry can
//! never outlive its wait, whatever queue it was moved to.
//!
//! Lock order: the table lock is taken before the task table (`block_task` and
//! `wake_task_with` lock it inside), never after.

use alloc::vec::Vec;

use spin::Mutex;

use crate::task::{self, WaitKind, WakeReason};

/// What a waiter waits on: an address space and a word in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub(super) space: u64,
    pub(super) addr: u64,
}

impl Key {
    /// The key of `addr` in the calling task's address space.
    pub(super) fn current(addr: u64) -> Key {
        Key {
            space: task::pml4_of(task::current()).unwrap_or(0),
            addr,
        }
    }
}

/// One parked task.
struct Waiter {
    key: Key,
    slot: usize,
    bitset: u32,
}

/// Every parked futex waiter, oldest first (wake order is FIFO).
static WAITERS: Mutex<Vec<Waiter>> = Mutex::new(Vec::new());

/// Park the calling task on `key` until a wake that matches `bitset`, the
/// `deadline` (monotonic ns), or a signal. The caller has already checked the word, with
/// interrupts off, so no wake can run between that check and this park.
pub(super) fn wait(key: Key, bitset: u32, deadline: Option<u64>) -> WakeReason {
    let slot = task::current();
    {
        let mut waiters = WAITERS.lock();
        waiters.push(Waiter { key, slot, bitset });
        task::block_task(slot, WaitKind::Futex, deadline);
    }
    task::switch::yield_now();
    loop {
        if let Some(reason) = task::take_wake_reason(slot) {
            // A wake already removed the entry; a timeout or a signal did not.
            remove_slot(slot);
            return reason;
        }
        task::nap();
    }
}

/// Drop every entry of `slot` (its wait ended).
fn remove_slot(slot: usize) {
    WAITERS.lock().retain(|waiter| waiter.slot != slot);
}

/// Wake up to `count` waiters on `key` whose bitset meets `bitset`. Returns
/// how many tasks were woken. A matching entry whose task is no longer blocked
/// (it timed out and has not resumed yet) is removed without counting, as
/// Linux does not count a waiter that was already leaving.
pub(super) fn wake(key: Key, bitset: u32, count: usize) -> usize {
    let mut woken = 0;
    let mut waiters = WAITERS.lock();
    let mut index = 0;
    while index < waiters.len() && woken < count {
        let waiter = &waiters[index];
        if waiter.key == key && waiter.bitset & bitset != 0 {
            let slot = waiter.slot;
            waiters.remove(index);
            if task::wake_task_with(slot, WakeReason::Woken) {
                woken += 1;
            }
        } else {
            index += 1;
        }
    }
    woken
}

/// Wake up to `wake_count` waiters on `from`, then move up to `move_count` of
/// the remaining ones to `to`. Returns `(woken, moved)`.
pub(super) fn requeue(from: Key, to: Key, wake_count: usize, move_count: usize) -> (usize, usize) {
    let woken = wake(from, u32::MAX, wake_count);
    let mut moved = 0;
    let mut waiters = WAITERS.lock();
    for waiter in waiters.iter_mut() {
        if moved == move_count {
            break;
        }
        if waiter.key == from {
            waiter.key = to;
            moved += 1;
        }
    }
    (woken, moved)
}

/// How many waiters are parked on `key` (tests and diagnostics).
#[allow(dead_code)]
pub(super) fn waiting_on(key: Key) -> usize {
    WAITERS.lock().iter().filter(|w| w.key == key).count()
}

/// Test hook: register `slot` as a waiter on `key` and block it, without the
/// calling task yielding (the harness runs every "waiter" from one task).
#[cfg(lazyos_tests)]
pub(super) fn park_for_test(slot: usize, key: Key, bitset: u32) {
    let mut waiters = WAITERS.lock();
    waiters.push(Waiter { key, slot, bitset });
    task::block_task(slot, WaitKind::Futex, None);
}

/// Test hook: forget `slot`'s entries (a test's cleanup).
#[cfg(lazyos_tests)]
pub(super) fn forget_for_test(slot: usize) {
    remove_slot(slot);
}

/// Test hook: total waiters in the table (a leak shows up as a count that
/// never returns to zero).
#[cfg(lazyos_tests)]
pub(super) fn total_for_test() -> usize {
    WAITERS.lock().len()
}
