//! The kernel's interrupts-off ticket lock: the entry lock (`sync::entry`)
//! and the shootdown sender lock (`arch::smp::shootdown`) are both one.
//!
//! Turns are first come, first served (a releasing CPU cannot win the next
//! turn back over a waiter), and the holder is a CPU, named by its area
//! index + 1, so a CPU can tell whether it holds the lock itself. Callers
//! spin with interrupts off, so the spin keeps the S8.2 rule for every such
//! wait: it serves this CPU's shootdown mailbox (the holder may be waiting
//! for this CPU's ack, and its doorbell cannot land) and halts if another
//! CPU is panicking (`smp::serve_pending`). A wait longer than the caller's
//! bound is a lock that will never be released, and calls its `timeout`.

use core::arch::x86_64::_rdtsc;
use core::sync::atomic::{AtomicU32, Ordering};

pub struct TicketLock {
    /// The next ticket and the ticket being served.
    next: AtomicU32,
    serving: AtomicU32,
    /// The holder's area index + 1 (0: nobody). Written only by the CPU that
    /// holds the lock (or is about to drop it), so a CPU that reads its own
    /// mark here holds the lock.
    holder: AtomicU32,
}

impl TicketLock {
    pub const fn new() -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
            holder: AtomicU32::new(0),
        }
    }

    /// Take the lock for `mine` (an area index + 1), with interrupts off.
    /// `bound` is the longest wait in TSC cycles (0: no bound); past it,
    /// `timeout(holder)` runs (it must not return). Returns how long this
    /// waited, 0 when the lock was free.
    pub fn lock(&self, mine: u32, bound: u64, timeout: impl Fn(u32) -> !) -> u64 {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let mut waited = 0;
        if self.serving.load(Ordering::Acquire) != ticket {
            waited = self.wait_for(ticket, bound, timeout);
        }
        self.holder.store(mine, Ordering::Relaxed);
        waited
    }

    /// Spin until `ticket` is served; the cycles it took.
    #[cold]
    fn wait_for(&self, ticket: u32, bound: u64, timeout: impl Fn(u32) -> !) -> u64 {
        let started = rdtsc();
        while self.serving.load(Ordering::Acquire) != ticket {
            crate::arch::smp::serve_pending();
            if bound != 0 && rdtsc().wrapping_sub(started) > bound {
                timeout(self.holder());
            }
            core::hint::spin_loop();
        }
        rdtsc().wrapping_sub(started).max(1)
    }

    /// Drop the lock (its holder only).
    pub fn unlock(&self) {
        self.holder.store(0, Ordering::Relaxed);
        // Release: what the holder wrote is visible to the next holder, which
        // loads `serving` with `Acquire`.
        self.serving.fetch_add(1, Ordering::Release);
    }

    /// The holder's area index + 1 (0: nobody).
    pub fn holder(&self) -> u32 {
        self.holder.load(Ordering::Relaxed)
    }

    /// Tickets taken and not yet served: the holder's and every waiter's
    /// (the suite waits on it to know a waiter has queued).
    #[cfg(lazyos_tests)]
    #[allow(dead_code)]
    pub fn outstanding(&self) -> u32 {
        self.next
            .load(Ordering::Acquire)
            .wrapping_sub(self.serving.load(Ordering::Acquire))
    }
}

/// The TSC read.
fn rdtsc() -> u64 {
    // SAFETY: reads the CPU's TSC; no memory or privilege effects.
    unsafe { _rdtsc() }
}
