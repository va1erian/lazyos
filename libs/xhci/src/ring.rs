//! Rings (xHCI 4.9): producer rings the driver fills (command, transfer) and
//! the event ring the controller fills.
//!
//! Ownership of a TRB is the cycle bit: a TRB belongs to the consumer while
//! its cycle bit equals the consumer's cycle state. A producer ring is one
//! segment whose last TRB is a Link back to the start with Toggle Cycle, so
//! every lap flips the bit the producer writes. The event ring is one segment
//! described by a one-entry Event Ring Segment Table; the consumer flips its
//! own cycle state when it wraps.

use core::sync::atomic::{fence, Ordering};

use crate::trb::{self, Trb, CYCLE, TRB_BYTES};
use crate::Error;

/// A segment of TRBs in DMA memory.
pub trait TrbMem {
    /// TRBs in the segment.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The segment's bus (physical) address, 64-byte aligned.
    fn phys(&self) -> u64;
    fn read(&self, index: usize) -> Trb;
    /// Write a TRB so the controller never sees a half-written one: the
    /// parameter and status first, then (after a release fence) the control
    /// word that carries the cycle bit.
    fn write(&mut self, index: usize, trb: Trb);
}

/// [`TrbMem`] over memory the driver mapped (syscall 23 `dma_alloc`).
pub struct RawMem {
    base: *mut u32,
    len: usize,
    phys: u64,
}

impl RawMem {
    /// # Safety
    ///
    /// `base` must point to `len` TRBs (16 bytes each) of DMA memory mapped
    /// for the life of the value, at bus address `phys`, used only through
    /// this value and the controller.
    pub unsafe fn new(base: *mut u8, len: usize, phys: u64) -> RawMem {
        RawMem {
            base: base.cast(),
            len,
            phys,
        }
    }
}

impl TrbMem for RawMem {
    fn len(&self) -> usize {
        self.len
    }

    fn phys(&self) -> u64 {
        self.phys
    }

    fn read(&self, index: usize) -> Trb {
        assert!(index < self.len);
        // SAFETY: `index < len`, so the four dwords are inside the region
        // `new`'s caller vouched for; volatile because the controller writes
        // them behind the compiler's back.
        unsafe {
            let at = self.base.add(index * 4);
            let lo = at.read_volatile();
            let hi = at.add(1).read_volatile();
            Trb {
                parameter: u64::from(lo) | u64::from(hi) << 32,
                status: at.add(2).read_volatile(),
                control: at.add(3).read_volatile(),
            }
        }
    }

    fn write(&mut self, index: usize, trb: Trb) {
        assert!(index < self.len);
        // SAFETY: as in `read`; the fence orders the body before the control
        // word the controller polls.
        unsafe {
            let at = self.base.add(index * 4);
            at.write_volatile(trb.parameter as u32);
            at.add(1).write_volatile((trb.parameter >> 32) as u32);
            at.add(2).write_volatile(trb.status);
            fence(Ordering::Release);
            at.add(3).write_volatile(trb.control);
        }
    }
}

/// The smallest ring accepted: one slot is the Link and one stays empty, so
/// eight leave room for two control transfers (three TRBs each).
pub const MIN_RING: usize = 8;

/// A command or transfer ring the driver produces into.
pub struct ProducerRing<M: TrbMem> {
    mem: M,
    /// Next slot to write.
    enqueue: usize,
    /// The cycle bit the next TRB is written with.
    cycle: bool,
    /// TRBs written and not yet retired, oldest first (a slot index each,
    /// recovered from the head and the count).
    head: usize,
    in_flight: usize,
}

impl<M: TrbMem> ProducerRing<M> {
    /// A fresh ring over `mem` (cleared to zero by the caller). The last slot
    /// becomes the Link TRB.
    pub fn new(mut mem: M) -> Result<ProducerRing<M>, Error> {
        if mem.len() < MIN_RING || !mem.phys().is_multiple_of(64) {
            return Err(Error::BadBuffer);
        }
        let last = mem.len() - 1;
        let phys = mem.phys();
        // The Link starts owned by nobody (cycle 0 = not the controller's
        // while the controller's cycle state is 1); it is handed over with
        // the right cycle when the producer reaches it.
        mem.write(last, trb::link(phys, true));
        Ok(ProducerRing {
            mem,
            enqueue: 0,
            cycle: true,
            head: 0,
            in_flight: 0,
        })
    }

    /// The ring's base address and initial cycle state, for `CRCR` or an
    /// endpoint context's TR Dequeue Pointer (bit 0 is the DCS).
    pub fn dequeue_pointer(&self) -> u64 {
        self.mem.phys() | u64::from(self.cycle)
    }

    /// Slots a caller can still fill.
    pub fn free(&self) -> usize {
        // One slot is the Link; one more is kept empty so a full ring never
        // looks empty to the controller.
        self.mem.len() - 2 - self.in_flight
    }

    /// Append TRBs as one unit (a control transfer is three); all or none.
    /// Every TRB but the last gets the Chain bit when `chain` is set. Returns
    /// the bus address of the last TRB, which its completion event names.
    pub fn enqueue(&mut self, trbs: &[Trb], chain: bool) -> Result<u64, Error> {
        if trbs.is_empty() || trbs.len() > self.free() {
            return Err(Error::RingFull);
        }
        let mut last = 0;
        for (n, trb) in trbs.iter().enumerate() {
            let mut trb = *trb;
            trb.control &= !CYCLE;
            if chain && n + 1 < trbs.len() {
                trb.control |= trb::CHAIN;
            }
            last = self.push(trb);
        }
        Ok(last)
    }

    fn push(&mut self, mut trb: Trb) -> u64 {
        if self.cycle {
            trb.control |= CYCLE;
        }
        let at = self.enqueue;
        self.mem.write(at, trb);
        self.in_flight += 1;
        self.enqueue += 1;
        if self.enqueue == self.mem.len() - 1 {
            // Hand the Link over with the current cycle, then flip.
            let mut link = trb::link(self.mem.phys(), true);
            if self.cycle {
                link.control |= CYCLE;
            }
            self.mem.write(self.enqueue, link);
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        self.mem.phys() + at as u64 * TRB_BYTES
    }

    /// The controller completed the TRB at `pointer` (from an event): retire
    /// it and everything enqueued before it. A pointer outside the ring,
    /// misaligned, or not in flight is refused.
    pub fn retire(&mut self, pointer: u64) -> Result<(), Error> {
        let index = self.index_of(pointer)?;
        let usable = self.mem.len() - 1;
        let distance = (index + usable - self.head) % usable;
        if distance >= self.in_flight {
            return Err(Error::BadPointer);
        }
        self.head = (index + 1) % usable;
        self.in_flight -= distance + 1;
        Ok(())
    }

    /// TRBs written and not yet completed.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    fn index_of(&self, pointer: u64) -> Result<usize, Error> {
        let offset = pointer
            .checked_sub(self.mem.phys())
            .ok_or(Error::BadPointer)?;
        let index = (offset / TRB_BYTES) as usize;
        if !offset.is_multiple_of(TRB_BYTES) || index >= self.mem.len() - 1 {
            return Err(Error::BadPointer);
        }
        Ok(index)
    }

    /// The underlying memory (tests and teardown).
    pub fn mem(&self) -> &M {
        &self.mem
    }
}

/// The event ring the controller produces into.
pub struct EventRing<M: TrbMem> {
    mem: M,
    dequeue: usize,
    cycle: bool,
}

/// An Event Ring Segment Table entry (6.5): segment address and size.
pub fn erst_entry(segment: u64, trbs: u16) -> [u32; 4] {
    [segment as u32, (segment >> 32) as u32, u32::from(trbs), 0]
}

impl<M: TrbMem> EventRing<M> {
    /// A fresh event ring over zeroed `mem` (16..=4096 TRBs).
    pub fn new(mem: M) -> Result<EventRing<M>, Error> {
        if !(16..=4096).contains(&mem.len()) || !mem.phys().is_multiple_of(64) {
            return Err(Error::BadBuffer);
        }
        Ok(EventRing {
            mem,
            dequeue: 0,
            cycle: true,
        })
    }

    /// The next event, if the controller has written one.
    pub fn pop(&mut self) -> Option<Trb> {
        let trb = self.mem.read(self.dequeue);
        if trb.cycle() != self.cycle {
            return None;
        }
        fence(Ordering::Acquire);
        self.dequeue += 1;
        if self.dequeue == self.mem.len() {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(trb)
    }

    /// The value for `ERDP` after a batch of pops: the dequeue position,
    /// with `EHB` set so the write also clears Event Handler Busy.
    pub fn erdp(&self) -> u64 {
        (self.mem.phys() + self.dequeue as u64 * TRB_BYTES) | crate::regs::rt::ERDP_EHB
    }

    /// The memory, writable: the tests play the controller through it.
    #[cfg(test)]
    pub(crate) fn mem_mut(&mut self) -> &mut M {
        &mut self.mem
    }

    /// The segment's address and size, for the ERST entry.
    pub fn segment(&self) -> (u64, u16) {
        (self.mem.phys(), self.mem.len() as u16)
    }
}
