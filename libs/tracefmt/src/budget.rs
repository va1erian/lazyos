//! A rate limit for a channel that can fire from a wedged peer or a tight
//! loop (`UI:STALL`, `LABEL:DENY`, `USBD:DIAG`): at most `lines` per
//! `window` of the caller's clock, then the excess is counted, and the first
//! line of the next window is preceded by one `SUPPRESSED` notice.
//!
//! It is the budget `xui-app/src/stall.rs` built for `UI:STALL`, made
//! lock-free (a kernel trace may fire where no lock is safe). Concurrent
//! chargers can overshoot the limit by a few lines at a window's edge; they
//! cannot lose the suppressed count or print two notices for one window.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// What a charge decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charge {
    /// Print the line.
    Allow,
    /// A new window: print a `SUPPRESSED n=<count>` notice, then the line.
    AllowAfter(u32),
    /// Over budget: drop the line (it is counted).
    Deny,
}

/// The state of one budget.
pub struct Budget {
    lines: u32,
    window: u64,
    /// Start of the current window, plus one (0: no window yet).
    begin: AtomicU64,
    printed: AtomicU32,
    suppressed: AtomicU32,
}

impl Budget {
    pub const fn new(lines: u32, window: u64) -> Budget {
        Budget {
            lines,
            window,
            begin: AtomicU64::new(0),
            printed: AtomicU32::new(0),
            suppressed: AtomicU32::new(0),
        }
    }

    /// Forget the window and the count (a test between two scenarios).
    pub fn reset(&self) {
        self.begin.store(0, Ordering::Release);
        self.printed.store(0, Ordering::Release);
        self.suppressed.store(0, Ordering::Release);
    }

    /// Charge one line at time `now`.
    pub fn charge(&self, now: u64) -> Charge {
        let begin = self.begin.load(Ordering::Acquire);
        let fresh = begin == 0 || now.saturating_sub(begin - 1) >= self.window;
        if fresh
            && self
                .begin
                .compare_exchange(
                    begin,
                    now.saturating_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            // This charger opened the window: it alone takes the count.
            let dropped = self.suppressed.swap(0, Ordering::AcqRel);
            self.printed.store(1, Ordering::Release);
            return if dropped > 0 {
                Charge::AllowAfter(dropped)
            } else {
                Charge::Allow
            };
        }
        if self.printed.fetch_add(1, Ordering::AcqRel) < self.lines {
            Charge::Allow
        } else {
            self.suppressed.fetch_add(1, Ordering::AcqRel);
            Charge::Deny
        }
    }
}
