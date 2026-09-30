//! The producing end of a frame ring.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::{Error, PushError, Region, LEN_BYTES, MAX_FRAME};

pub struct Producer {
    region: Region,
    /// Our index, private. The header copy is write-only for us, so a peer
    /// scribbling on it changes what the peer sees, never what we do.
    head: u32,
    poisoned: bool,
}

// SAFETY: the endpoint owns its private index and a pointer to memory the
// caller vouched for at construction; moving it between threads is fine as long
// as it is used from one at a time, which `&mut self` on every operation
// enforces.
unsafe impl Send for Producer {}

impl Producer {
    pub(crate) fn new(region: Region, head: u32) -> Producer {
        Producer {
            region,
            head,
            poisoned: false,
        }
    }

    /// Frames in flight according to the peer's tail, or `Corrupt` when the
    /// peer's index cannot be right. Reads the shared tail exactly once.
    fn in_flight(&mut self) -> Result<u32, Error> {
        if self.poisoned {
            return Err(Error::Corrupt);
        }
        let tail = self.region.tail().load(Ordering::Acquire);
        let used = self.head.wrapping_sub(tail);
        if used > self.region.slots() {
            self.poisoned = true;
            return Err(Error::Corrupt);
        }
        Ok(used)
    }

    /// Slots currently free.
    pub fn free(&mut self) -> Result<u32, Error> {
        Ok(self.region.slots() - self.in_flight()?)
    }

    /// Copy `frame` into the next slot and publish it. A frame is never
    /// truncated: an empty or over-long one is refused, and a full ring returns
    /// [`PushError::Full`] with nothing written.
    pub fn push(&mut self, frame: &[u8]) -> Result<(), PushError> {
        if frame.is_empty() {
            return Err(PushError::Empty);
        }
        if frame.len() > MAX_FRAME {
            return Err(PushError::TooLong);
        }
        let used = self.in_flight().map_err(|_| PushError::Corrupt)?;
        if used >= self.region.slots() {
            return Err(PushError::Full);
        }
        let slot = self.region.slot(self.head);
        // SAFETY: `slot` points at a whole slot of SLOT_BYTES inside the region,
        // and `LEN_BYTES + frame.len() <= SLOT_BYTES` (checked above). The
        // consumer may read the slot concurrently only after the release store
        // of `head` below, and a hostile one that reads early sees garbage, which
        // it must tolerate anyway.
        unsafe {
            ptr::write_volatile(slot as *mut u16, (frame.len() as u16).to_le());
            ptr::copy_nonoverlapping(frame.as_ptr(), slot.add(LEN_BYTES), frame.len());
        }
        self.head = self.head.wrapping_add(1);
        self.region.head().store(self.head, Ordering::Release);
        Ok(())
    }

    /// After a burst of pushes: whether the consumer armed the ring and so
    /// wants a wake-up message. Clears the flag in the same atomic step, so a
    /// burst yields at most one wake-up per arming.
    pub fn take_notify(&mut self) -> bool {
        self.region.armed().swap(0, Ordering::SeqCst) != 0
    }

    /// Slots in the ring.
    pub fn slots(&self) -> u32 {
        self.region.slots()
    }

    /// Whether an earlier call found the peer's index impossible.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Frames this endpoint has published in total (wrapping), for tests and
    /// counters.
    pub fn published(&self) -> u32 {
        self.head
    }
}
