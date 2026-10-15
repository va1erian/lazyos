//! What the serial port says about itself (issue #857): the bytes its ring
//! dropped.
//!
//! The line is a trace, and a trace takes the port's lock, so [`report`] is
//! called with interrupts **off** from task context (the multiplexer loop, as
//! a kernel section): a task preempted while it held the lock would keep it
//! from every syscall that logs with interrupts off (the #382 rule).

use core::sync::atomic::{AtomicU64, Ordering};

use super::{port, tx};

/// Bytes ever dropped for lack of room (counted when reported).
static DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// `SERIAL:DROPPED`, if due.
pub fn report() {
    report_dropped();
}

/// Bytes dropped for lack of room since boot, reported or not.
#[cfg(lazyos_tests)]
pub fn dropped_total() -> u64 {
    let pending = x86_64::instructions::interrupts::without_interrupts(|| {
        port().as_ref().map_or(0, |port| port.tx.pending_drops())
    });
    DROPPED_TOTAL.load(Ordering::Relaxed) + pending as u64
}

/// Report the bytes dropped while the ring was full, once it has room again:
/// `SERIAL:DROPPED bytes=<n> total=<n>`. A flood that never lets up is told
/// once in a while, with the total.
pub fn report_dropped() {
    static BUDGET: tracefmt::Budget = tracefmt::Budget::new(3, 1000);
    let taken = x86_64::instructions::interrupts::without_interrupts(|| match port().as_mut() {
        Some(port)
            if port.tx.len() < tx::Tx::CAPACITY / 2
                && port.tx.pending_drops() > 0
                && BUDGET.charge(crate::task::ticks()) != tracefmt::Charge::Deny =>
        {
            port.tx.take_dropped()
        }
        _ => 0,
    });
    if taken == 0 {
        return;
    }
    let total = DROPPED_TOTAL.fetch_add(taken as u64, Ordering::Relaxed) + taken as u64;
    crate::trace::event!(
        tracefmt::Channel::Serial,
        "DROPPED",
        bytes = taken,
        total = total
    );
}

/// The drops not yet reported and the total with them, taken for the last
/// line the machine prints (a panic, a power-off), where no budget applies.
pub(super) fn take_for_final() -> Option<(usize, u64)> {
    let taken = x86_64::instructions::interrupts::without_interrupts(|| {
        port().as_mut().map_or(0, |port| port.tx.take_dropped())
    });
    if taken == 0 {
        return None;
    }
    let total = DROPPED_TOTAL.fetch_add(taken as u64, Ordering::Relaxed) + taken as u64;
    Some((taken, total))
}
