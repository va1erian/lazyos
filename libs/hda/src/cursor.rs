//! Turning a cyclic link position into completed periods.
//!
//! An HDA stream plays its buffer round and round with no notion of "this
//! period was submitted": the driver learns how far it got from the link
//! position (`LPIB`) and counts the periods behind it. The cursor accumulates
//! the forward distance between two readings modulo the buffer, so it must be
//! read at least once per buffer cycle (the driver reads it every tick while a
//! stream runs, and a cycle is several ticks). A position outside the buffer
//! is a lying controller and is ignored.

/// Progress through a cyclic buffer of `periods` periods of `period` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    buffer: u32,
    period: u32,
    last: u32,
    /// Bytes played since [`Cursor::new`].
    played: u64,
    /// Periods already reported complete.
    reported: u64,
}

impl Cursor {
    /// A cursor at position 0. `None` for a zero period or a buffer that is
    /// not a whole number of periods.
    pub fn new(period: u32, periods: u32) -> Option<Cursor> {
        let buffer = period.checked_mul(periods)?;
        (period > 0 && periods > 0).then_some(Cursor {
            buffer,
            period,
            last: 0,
            played: 0,
            reported: 0,
        })
    }

    /// Feed a new link position; returns how many more periods are now
    /// complete. Completed periods are numbered from 0 in play order, so period
    /// `n` is buffer slot `n % periods`.
    pub fn advance(&mut self, position: u32) -> u32 {
        if position >= self.buffer {
            return 0;
        }
        let forward = (position + self.buffer - self.last) % self.buffer;
        self.last = position;
        self.played += u64::from(forward);
        let complete = self.played / u64::from(self.period);
        let new = complete - self.reported;
        self.reported = complete;
        new as u32
    }

    /// Periods reported complete so far.
    pub fn completed(&self) -> u64 {
        self.reported
    }

    /// The buffer slot the next period to complete plays from.
    pub fn next_slot(&self) -> u32 {
        (self.reported % u64::from(self.buffer / self.period)) as u32
    }
}
