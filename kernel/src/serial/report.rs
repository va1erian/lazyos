//! What the serial port says about itself (issues #857, #861): the bytes its
//! ring dropped, and how far behind the port is.
//!
//! Both are traces, and a trace takes the port's lock, so [`report`] is
//! called with interrupts **off** from task context (the multiplexer loop, as
//! a kernel section): a task preempted while it held the lock would keep it
//! from every syscall that logs with interrupts off (the #382 rule).

use core::sync::atomic::{AtomicU64, Ordering};

use super::{port, tx};

/// Ring fill above which `SERIAL:BACKLOG` speaks.
const BACKLOG_REPORT_BYTES: usize = 4096;
/// Ticks between two looks at the backlog (and so between two lines).
const BACKLOG_REPORT_TICKS: u64 = 500;

/// Bytes ever dropped for lack of room (counted when reported).
static DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Tick of the last look at the backlog.
static BACKLOG_LAST: AtomicU64 = AtomicU64::new(0);

/// `SERIAL:DROPPED` and `SERIAL:BACKLOG`, as due.
pub fn report() {
    report_dropped();
    report_backlog_at(crate::task::ticks());
}

/// Bytes dropped for lack of room since boot, reported or not.
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

/// Every [`BACKLOG_REPORT_TICKS`], if the ring holds more than 4 KiB:
/// `SERIAL:BACKLOG bytes=<n> dropped=<total> max_io_us=<n>`: how far behind
/// the port is, what it has refused, and the longest single access to it
/// *since the last look* (a host that is slow to take bytes shows here before
/// anything else does). The access maximum is taken at every look, line or
/// not, so one slow access an hour ago does not stand in a later line.
pub(super) fn report_backlog_at(now: u64) {
    if now.wrapping_sub(BACKLOG_LAST.load(Ordering::Relaxed)) < BACKLOG_REPORT_TICKS {
        return;
    }
    BACKLOG_LAST.store(now, Ordering::Relaxed);
    let (bytes, cycles) =
        x86_64::instructions::interrupts::without_interrupts(|| match port().as_mut() {
            Some(port) => {
                let cycles = port.tx.take_max_io();
                (port.tx.len(), cycles)
            }
            None => (0, 0),
        });
    if bytes <= BACKLOG_REPORT_BYTES {
        return;
    }
    crate::trace::event!(
        tracefmt::Channel::Serial,
        "BACKLOG",
        bytes = bytes,
        dropped = dropped_total(),
        max_io_us = crate::arch::irqoff::to_us(cycles)
    );
}

/// Test builds: forget when the backlog was last looked at.
#[cfg(lazyos_tests)]
pub(super) fn forget_throttle() {
    BACKLOG_LAST.store(0, Ordering::Relaxed);
}
