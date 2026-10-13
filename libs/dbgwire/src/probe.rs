//! Hardware reads on the wire: the per-connection byte budget.
//!
//! `pci.config` and `mmio.read` ask the kernel to touch a device, so a
//! client that loops on them is a load on the bus, not only on `dbgd`. A
//! token bucket (bytes, refilled with time) bounds the sustained rate while
//! allowing a burst: dumping a few kilobytes of registers is immediate, a
//! 8 MiB GTT dump takes about half a minute.

/// Bytes a client may read in a burst.
pub const BURST_BYTES: u64 = 512 * 1024;
/// Bytes of budget returned per second.
pub const REFILL_PER_SEC: u64 = 256 * 1024;
/// The least one request costs (a 4-byte read is still a bus transaction
/// and a round trip).
pub const MIN_COST: u64 = 64;

/// A byte budget refilled by the clock.
#[derive(Clone, Debug)]
pub struct Budget {
    tokens: u64,
    last_ms: u64,
}

impl Budget {
    /// A full bucket at time `now_ms`.
    pub fn new(now_ms: u64) -> Budget {
        Budget {
            tokens: BURST_BYTES,
            last_ms: now_ms,
        }
    }

    /// Take the cost of reading `bytes` at `now_ms`, or say how many
    /// milliseconds until it could be afforded.
    pub fn take(&mut self, now_ms: u64, bytes: u64) -> Result<(), u64> {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        self.last_ms = now_ms;
        self.tokens = self
            .tokens
            .saturating_add(elapsed.saturating_mul(REFILL_PER_SEC) / 1000)
            .min(BURST_BYTES);
        let cost = bytes.max(MIN_COST);
        if cost <= self.tokens {
            self.tokens -= cost;
            return Ok(());
        }
        let missing = cost - self.tokens;
        Err((missing * 1000).div_ceil(REFILL_PER_SEC))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_then_the_refill_rate() {
        let mut budget = Budget::new(1000);
        // The whole burst is available at once.
        assert_eq!(budget.take(1000, BURST_BYTES), Ok(()));
        // Then nothing, until time passes: 4 KiB is 16 ms at 256 KiB/s.
        assert_eq!(budget.take(1000, 4096), Err(16));
        assert_eq!(budget.take(1008, 4096), Err(8));
        assert_eq!(budget.take(1016, 4096), Ok(()));
    }

    #[test]
    fn a_tiny_read_still_costs_something() {
        let mut budget = Budget::new(0);
        let mut taken = 0;
        while budget.take(0, 4).is_ok() {
            taken += 1;
        }
        assert_eq!(taken, BURST_BYTES / MIN_COST);
    }

    #[test]
    fn the_bucket_never_overfills_or_goes_backwards() {
        let mut budget = Budget::new(10_000);
        // A clock that ran backwards is not credit.
        assert_eq!(budget.take(5, 100), Ok(()));
        // A long idle refills to the burst and no further.
        assert_eq!(budget.take(10_000_000, BURST_BYTES), Ok(()));
        assert!(budget.take(10_000_000, 1).is_err());
        // A request larger than the bucket can never be afforded: the caller
        // is told a (long) delay, not a panic.
        assert!(budget.take(10_000_000, BURST_BYTES * 2).is_err());
    }
}
