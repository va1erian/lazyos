//! The pump's doorbell (docs/performance-plan.md P4.1).
//!
//! `netd` used to look at the `AF_INET` table on a timer (every tick while a
//! socket was open, every five otherwise), so a `connect` waited up to 50 ms
//! for its request to be seen. Now everything an application does that
//! `netd` must act on rings this bell, and `netd` parks on it beside its
//! Messenger endpoint (`channels::wait_any` with `WAIT_INET`):
//!
//! * a request is queued (bind, connect, listen, close);
//! * the application writes into an empty send ring (an edge: while the ring
//!   holds bytes `netd` has not taken, it is already due to look again);
//! * the application reads from a receive ring that had less than
//!   [`LOW_SPACE`] free (the only case in which `netd` can be holding bytes
//!   it could not deliver);
//! * the application drops either ring (close or `shutdown`).
//!
//! `netd`'s own reads and writes never ring, so the pump cannot wake itself.
//!
//! The bell is one pending flag plus the parked waiter, both atomics: it is
//! rung from syscall context (interrupts off, one CPU) with no lock of this
//! module held, and arming takes no lock either, so it nests under nothing.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Free bytes below which a read from a receive ring rings the bell: a
/// datagram message with its address (1478 bytes) fits in anything above.
pub const LOW_SPACE: usize = 2048;

const NO_TASK: usize = usize::MAX;

/// Something happened since `netd` last armed the bell.
static PENDING: AtomicBool = AtomicBool::new(false);
/// `netd`, while it is parked on the bell.
static WAITER: AtomicUsize = AtomicUsize::new(NO_TASK);
/// The attached `netd` (mirrors the table's, readable without its lock).
static OWNER: AtomicUsize = AtomicUsize::new(NO_TASK);
/// Rings over the life of the kernel (tests and soak accounting).
static RINGS: AtomicU64 = AtomicU64::new(0);

/// Ring the bell: mark work pending and wake `netd` if it is parked on it.
/// Takes the Messenger queue and task-table locks to wake: call with no
/// lock held that those may nest under (none of this module's).
pub fn ring() {
    RINGS.fetch_add(1, Ordering::Relaxed);
    PENDING.store(true, Ordering::Release);
    let waiter = WAITER.swap(NO_TASK, Ordering::AcqRel);
    if waiter != NO_TASK {
        crate::ipc::channels::wake_parked(waiter);
    }
}

/// `me` arms the bell before parking. `Ok(true)`: something is pending
/// already (consumed; nothing is armed). `Err` when `me` is not the attached
/// `netd`.
pub fn arm(me: usize) -> Result<bool, ()> {
    if OWNER.load(Ordering::Acquire) != me || me == NO_TASK {
        return Err(());
    }
    if PENDING.swap(false, Ordering::AcqRel) {
        return Ok(true);
    }
    WAITER.store(me, Ordering::Release);
    Ok(false)
}

/// Withdraw `me`'s registration, if it is still armed.
pub fn disarm(me: usize) {
    let _ = WAITER.compare_exchange(me, NO_TASK, Ordering::AcqRel, Ordering::Acquire);
}

/// A new `netd` attached (or none, `None`): forget the old one's state.
pub(super) fn set_owner(slot: Option<usize>) {
    OWNER.store(slot.unwrap_or(NO_TASK), Ordering::Release);
    WAITER.store(NO_TASK, Ordering::Release);
    PENDING.store(false, Ordering::Release);
}

/// Times the bell has rung.
pub fn rings() -> u64 {
    RINGS.load(Ordering::Relaxed)
}

/// Whether a task is parked on the bell (tests).
pub fn armed() -> bool {
    WAITER.load(Ordering::Acquire) != NO_TASK
}
