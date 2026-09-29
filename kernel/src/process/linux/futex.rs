//! `futex(uaddr, op, val)` — only `FUTEX_WAIT`/`FUTEX_WAKE` (and their
//! `_BITSET` variants, treated the same since nothing here uses bitsets): the
//! primitive musl's mutexes and thread-join build on.

use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;

use crate::task::wait::WaitQueue;
use crate::task::{self, WaitKind, WakeReason};
use crate::user_ptr;

use super::errno::{err, EAGAIN, EINTR, ETIMEDOUT};

/// Futex word address -> wait queue. A waiter parks on the queue keyed by its
/// word; `FUTEX_WAKE` notifies exactly that queue, so a wake cannot reach an
/// unrelated futex. Queue entries are pruned once empty and unreferenced.
static FUTEX_QUEUES: Mutex<Vec<(u64, Arc<WaitQueue>)>> = Mutex::new(Vec::new());

/// `futex(uaddr, op, val)` — only WAIT/WAKE (the mutex/join primitives).
pub(super) fn sys_futex(uaddr: u64, op: u64, val: u64) -> u64 {
    match op & 0x7f {
        0 | 9 => futex_wait(uaddr, val),  // FUTEX_WAIT / FUTEX_WAIT_BITSET
        1 | 10 => futex_wake(uaddr, val), // FUTEX_WAKE / FUTEX_WAKE_BITSET
        _ => 0,
    }
}

/// The wait queue for a futex word, created on first use.
fn futex_queue(uaddr: u64) -> Arc<WaitQueue> {
    let mut queues = FUTEX_QUEUES.lock();
    if let Some((_, queue)) = queues.iter().find(|(address, _)| *address == uaddr) {
        return Arc::clone(queue);
    }
    let queue = Arc::new(WaitQueue::new(WaitKind::Futex));
    queues.push((uaddr, Arc::clone(&queue)));
    queue
}

fn futex_wait(uaddr: u64, val: u64) -> u64 {
    // Safety: the futex word is a user 32-bit value (the syscall ABI's contract).
    let current = unsafe { user_ptr::read::<u32>(uaddr) };
    if current != val as u32 {
        return err(EAGAIN); // value changed: nothing to wait for
    }
    // The value check above and the park below are atomic with respect to
    // wakers: interrupts are masked in the syscall gate, and the single CPU
    // cannot run a `FUTEX_WAKE` between them.
    let queue = futex_queue(uaddr);
    match queue.wait(task::current(), None) {
        WakeReason::Woken => 0,
        WakeReason::TimedOut => err(ETIMEDOUT),
        WakeReason::Interrupted => err(EINTR),
    }
}

/// Wake up to `count` waiters on the futex at `uaddr`. Used both by
/// `sys_futex` directly and by [`super::procctl`]'s thread-exit path, which
/// futex-wakes a joiner after clearing `clear_child_tid`.
pub(super) fn futex_wake(uaddr: u64, count: u64) -> u64 {
    let mut queues = FUTEX_QUEUES.lock();
    let Some(position) = queues.iter().position(|(address, _)| *address == uaddr) else {
        return 0;
    };
    let woken = if count == 1 {
        queues[position].1.notify_one()
    } else {
        queues[position]
            .1
            .notify(count.min(task::MAX_TASKS as u64) as usize)
    };
    // Prune empty queues so dead word addresses do not accumulate. Waiters hold
    // an `Arc` clone for as long as they are parked (and until their wait
    // returns), so the strong count guard keeps a queue alive while a lookup or
    // wake is still in flight.
    if queues[position].1.is_empty() && Arc::strong_count(&queues[position].1) == 1 {
        queues.remove(position);
    }
    woken as u64
}
