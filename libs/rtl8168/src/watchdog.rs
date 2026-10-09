//! The transmit-hang rule (docs/rtl8168-driver-plan.md section 3.4): the
//! Realtek family is known for transmit queues that stop and only a reset
//! revives, so a ring with frames queued and no completion for
//! [`TIMEOUT_TICKS`] while the link is up is fatal, and the restart's soft
//! reset is the recovery.
//!
//! Pure logic over the clock the binary passes in, so the rule is host-tested
//! without a chip.

use nicdrv::Fatal;

/// 5 seconds at the 100 Hz tick.
pub const TIMEOUT_TICKS: u64 = 500;

#[derive(Clone, Copy, Debug, Default)]
pub struct TxWatchdog {
    /// The completion count last seen, and when it last moved (or the queue
    /// last had nothing to wait for).
    reaped: u64,
    since: u64,
}

impl TxWatchdog {
    pub fn new(now: u64) -> TxWatchdog {
        TxWatchdog {
            reaped: 0,
            since: now,
        }
    }

    /// Judge the transmit queue at tick `now`: `in_flight` frames queued,
    /// `reaped` frames completed since the start, whether the link is up.
    pub fn check(
        &mut self,
        now: u64,
        in_flight: u16,
        reaped: u64,
        link_up: bool,
    ) -> Result<(), Fatal> {
        if in_flight == 0 || !link_up || reaped != self.reaped {
            self.reaped = reaped;
            self.since = now;
            return Ok(());
        }
        if now.saturating_sub(self.since) >= TIMEOUT_TICKS {
            return Err(Fatal::Hardware("tx timeout"));
        }
        Ok(())
    }
}
