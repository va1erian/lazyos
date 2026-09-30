//! The consuming end of a frame ring.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::{Error, FrameBuf, PopError, Region, LEN_BYTES, MAX_FRAME};

pub struct Consumer {
    region: Region,
    /// Our index, private (see `Producer::head`).
    tail: u32,
    poisoned: bool,
}

// SAFETY: as for `Producer`.
unsafe impl Send for Consumer {}

impl Consumer {
    pub(crate) fn new(region: Region, tail: u32) -> Consumer {
        Consumer {
            region,
            tail,
            poisoned: false,
        }
    }

    /// Frames the peer says are waiting, or `Corrupt` when its index claims
    /// more than the ring holds. Reads the shared head exactly once.
    pub fn pending(&mut self) -> Result<u32, Error> {
        if self.poisoned {
            return Err(Error::Corrupt);
        }
        let head = self.region.head().load(Ordering::Acquire);
        let avail = head.wrapping_sub(self.tail);
        if avail > self.region.slots() {
            self.poisoned = true;
            return Err(Error::Corrupt);
        }
        Ok(avail)
    }

    /// Take the next frame, copying it into `out` before returning its length,
    /// so the caller parses a private copy the producer cannot rewrite.
    ///
    /// * `Ok(Some(n))`: `out[..n]` holds a frame of `1..=MAX_FRAME` bytes (a
    ///   producer can also publish a zero-length slot; that is delivered as
    ///   `Some(0)` and left to the caller to count as a runt);
    /// * `Ok(None)`: the ring is empty;
    /// * `Err(PopError::BadLength(n))`: the slot claimed `n > MAX_FRAME` bytes.
    ///   The slot is consumed and nothing is copied;
    /// * `Err(PopError::Corrupt)`: the peer's index is impossible; the endpoint
    ///   is poisoned for good.
    pub fn pop(&mut self, out: &mut FrameBuf) -> Result<Option<usize>, PopError> {
        let avail = self.pending().map_err(|_| PopError::Corrupt)?;
        if avail == 0 {
            return Ok(None);
        }
        let slot = self.region.slot(self.tail);
        // SAFETY: `slot` is a whole in-range slot. The length is read once with
        // a volatile load; everything after uses that one value.
        let claimed = u16::from_le(unsafe { ptr::read_volatile(slot as *const u16) });
        let len = usize::from(claimed);
        let result = if len > MAX_FRAME {
            Err(PopError::BadLength(claimed))
        } else {
            // SAFETY: `len <= MAX_FRAME`, so the source range lies inside the
            // slot and the destination inside `out`; they cannot overlap
            // (`out` is private memory). The producer may rewrite the source
            // meanwhile, which yields a garbled frame, never an out-of-bounds
            // access.
            unsafe { ptr::copy_nonoverlapping(slot.add(LEN_BYTES), out.as_mut_ptr(), len) };
            Ok(Some(len))
        };
        self.tail = self.tail.wrapping_add(1);
        self.region.tail().store(self.tail, Ordering::Release);
        result
    }

    /// Ask for a wake-up: set the `armed` flag. Call it *before* a final
    /// [`Consumer::pending`] check and only sleep if that finds nothing, so a
    /// frame published in between is never missed.
    pub fn arm(&mut self) {
        self.region.armed().store(1, Ordering::SeqCst);
    }

    /// Slots in the ring.
    pub fn slots(&self) -> u32 {
        self.region.slots()
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Frames this endpoint has consumed in total (wrapping).
    pub fn consumed(&self) -> u32 {
        self.tail
    }
}
