//! The shutdown watchdog (docs/shutdown.md, "Kernel hardening"): the kernel's
//! backstop for an orderly shutdown that never reaches `power`.
//!
//! `init` arms it ([`super::ARM_WATCHDOG`]) when a shutdown starts. If the
//! machine is still running [`TIMEOUT_TICKS`] later -- `init` hung on a
//! service, crashed, or was killed -- the kernel task (the mux, which polls
//! [`service`] every frame) kills every user task, syncs the filesystems and
//! performs the stop `init` asked for. It is one-way: nothing disarms it, and
//! a second arm never moves the first deadline, so a stalled supervisor cannot
//! keep postponing the stop.
//!
//! The whole state is one word, so arming and firing are single atomic steps:
//! `0` is disarmed, [`FIRED`] means it has acted, anything else is
//! `deadline << 1 | op`.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::task;

/// How long after arming the kernel forces the stop: 30 s at 100 Hz, well
/// past `init`'s own 20 s global deadline.
pub const TIMEOUT_TICKS: u64 = 3000;

/// The state word once the watchdog has fired (never re-armed after).
const FIRED: u64 = u64::MAX;

static STATE: AtomicU64 = AtomicU64::new(0);

/// Pack an armed state. `op` is [`super::REBOOT`] (0) or
/// [`super::SHUTDOWN`] (1); callers validate it first.
fn pack(deadline: u64, op: u64) -> u64 {
    (deadline << 1) | (op & 1)
}

/// The armed `(deadline, op)`, if any.
pub fn armed() -> Option<(u64, u64)> {
    match STATE.load(Ordering::Acquire) {
        0 | FIRED => None,
        word => Some((word >> 1, word & 1)),
    }
}

/// Arm for `op` at `now`. Returns whether this call armed it; a watchdog that
/// is already armed (or has fired) keeps its deadline and op.
pub fn arm(op: u64, now: u64) -> bool {
    // Bounded so the shifted deadline cannot overflow or collide with `FIRED`.
    let deadline = now.saturating_add(TIMEOUT_TICKS).clamp(1, u64::MAX >> 2);
    let armed = STATE
        .compare_exchange(0, pack(deadline, op), Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    if armed {
        crate::serial_println!(
            "power: watchdog armed ({}, deadline tick {})",
            name(op),
            deadline
        );
    }
    armed
}

/// The op to force when the deadline has passed at `now`, exactly once: the
/// first caller to see it expired gets it, everyone after gets `None`.
pub fn take_expired(now: u64) -> Option<u64> {
    let (deadline, op) = armed()?;
    if now < deadline {
        return None;
    }
    STATE
        .compare_exchange(
            pack(deadline, op),
            FIRED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .ok()
        .map(|_| op)
}

/// Called by the kernel task every frame: force the stop once the deadline
/// passed.
pub fn service() {
    let Some(op) = take_expired(task::ticks()) else {
        return;
    };
    crate::serial_println!("power: watchdog expired; forcing {}", name(op));
    #[cfg(not(lazyos_tests))]
    force(op);
}

/// How long the killed tasks get to be torn down before the sync (ticks).
#[cfg(not(lazyos_tests))]
const SETTLE_TICKS: u64 = 10;

/// Kill every user task, give the scheduler a moment to reap them, then sync
/// and stop. Runs on the kernel task, which may sleep.
#[cfg(not(lazyos_tests))]
fn force(op: u64) -> ! {
    use crate::task::signal::{self, SigInfo};
    // Nothing must be writing while the filesystems sync. `-ESRCH` (no user
    // task left) is fine.
    let _ = signal::kill(task::KERNEL_TASK, -1, signal::SIGKILL, SigInfo::kernel());
    task::idle(task::ticks() + SETTLE_TICKS);
    super::stop(op)
}

/// The op's name, for the log.
fn name(op: u64) -> &'static str {
    if op == super::REBOOT {
        "reboot"
    } else {
        "power-off"
    }
}

/// Disarm, for test isolation only: a running system never disarms.
#[cfg(lazyos_tests)]
pub fn reset_for_tests() {
    STATE.store(0, Ordering::Release);
}
