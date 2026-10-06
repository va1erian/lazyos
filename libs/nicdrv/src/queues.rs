//! The receive and transmit virtqueues over one DMA block.
//!
//! Every descriptor owns one fixed 2048-byte slot (`virtio_net::SLOT_BYTES`),
//! so the device only ever sees driver-owned memory at driver-chosen
//! addresses, and a completion names a slot through a table the driver wrote.
//! Everything the device reports back (used ids, lengths) is untrusted: an id
//! that is not in flight is a [`Fatal`](crate::Fatal) error, a length beyond
//! the slot is reported to the caller and never used to index.
//!
//! A receive buffer is **copied out of the slot before it is parsed**, and the
//! slot is handed straight back to the device afterwards, so a device that
//! rewrites a completed buffer changes nothing the driver examines.

use core::ptr;

use virtio::queue::{Buf, Layout as QueueLayout, Virtqueue, MAX_QUEUE};
use virtio_net::hdr::{NetHdr, HDR_LEN};
use virtio_net::SLOT_BYTES;

use crate::rings::{NicRings, RxError};
use crate::Fatal;

const NONE: u16 = u16::MAX;
const PAGE: usize = 4096;

/// A physically contiguous block the driver maps and the device addresses.
#[derive(Clone, Copy, Debug)]
pub struct DmaBlock {
    va: *mut u8,
    bus: u64,
    len: usize,
}

impl DmaBlock {
    /// Describe a block.
    ///
    /// # Safety
    /// `va` must be valid for reads and writes of `len` bytes, 4-byte aligned,
    /// mapped for as long as any [`Queues`] built on it exists, and `bus` must
    /// be the address the device uses for the same bytes. The driver must not
    /// touch the memory except through [`Queues`] while the device runs.
    pub unsafe fn new(va: *mut u8, bus: u64, len: usize) -> DmaBlock {
        DmaBlock { va, bus, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn va(&self) -> *mut u8 {
        self.va
    }

    pub fn bus(&self) -> u64 {
        self.bus
    }
}

/// Where everything sits in the block. Offsets are page aligned so a queue
/// never shares a page with a packet slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub rx_entries: u16,
    pub tx_entries: u16,
    pub rx_queue: usize,
    pub tx_queue: usize,
    pub rx_slots: usize,
    pub tx_slots: usize,
    pub total: usize,
}

fn round_up(value: usize) -> usize {
    value.div_ceil(PAGE) * PAGE
}

impl Layout {
    /// The layout for `rx_entries` receive and `tx_entries` transmit slots;
    /// `None` when either is not a power of two between 2 and
    /// [`MAX_QUEUE`].
    pub fn new(rx_entries: u16, tx_entries: u16) -> Option<Layout> {
        let legal = |n: u16| n >= 2 && usize::from(n) <= MAX_QUEUE && n.is_power_of_two();
        if !legal(rx_entries) || !legal(tx_entries) {
            return None;
        }
        let rx_queue = 0;
        let tx_queue = rx_queue + round_up(QueueLayout::new(rx_entries).total);
        let rx_slots = tx_queue + round_up(QueueLayout::new(tx_entries).total);
        let tx_slots = rx_slots + usize::from(rx_entries) * SLOT_BYTES;
        let total = tx_slots + usize::from(tx_entries) * SLOT_BYTES;
        Some(Layout {
            rx_entries,
            tx_entries,
            rx_queue,
            tx_queue,
            rx_slots,
            tx_slots,
            total,
        })
    }
}

/// Why [`Queues::tx_send`] refused a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    /// Every transmit slot is in flight; retry after the device returns some.
    NoSlot,
    /// Longer than a slot can hold after the packet header.
    TooLong,
}

/// The receive and transmit queues and their slot pools.
pub struct Queues {
    block: DmaBlock,
    layout: Layout,
    rx: Virtqueue,
    tx: Virtqueue,
    /// Slot of each in-flight receive head (`NONE` when not in flight).
    rx_slot: [u16; MAX_QUEUE],
    tx_slot: [u16; MAX_QUEUE],
    /// Stack of free transmit slots.
    tx_free: [u16; MAX_QUEUE],
    tx_free_len: u16,
    /// Private copy of the completion being examined.
    scratch: [u8; SLOT_BYTES],
}

impl Queues {
    /// Build both queues and the slot pools over `block`, zeroing the queue
    /// memory. Nothing is posted to the device yet.
    ///
    /// # Safety
    /// As [`DmaBlock::new`]: the block must outlive the queues and be
    /// exclusively theirs.
    pub unsafe fn new(block: DmaBlock, rx_entries: u16, tx_entries: u16) -> Result<Queues, Fatal> {
        let layout = Layout::new(rx_entries, tx_entries).ok_or(Fatal::Layout)?;
        if block.len < layout.total || !(block.va as usize).is_multiple_of(PAGE) {
            return Err(Fatal::Layout);
        }
        // SAFETY: each queue window is inside the block (`total` fits), 4-byte
        // aligned (page aligned), and only that queue touches it.
        let rx = unsafe {
            Virtqueue::new(
                block.va.add(layout.rx_queue),
                block.bus + layout.rx_queue as u64,
                rx_entries,
            )
        }?;
        // SAFETY: as above.
        let tx = unsafe {
            Virtqueue::new(
                block.va.add(layout.tx_queue),
                block.bus + layout.tx_queue as u64,
                tx_entries,
            )
        }?;
        let mut queues = Queues {
            block,
            layout,
            rx,
            tx,
            rx_slot: [NONE; MAX_QUEUE],
            tx_slot: [NONE; MAX_QUEUE],
            tx_free: [0; MAX_QUEUE],
            tx_free_len: 0,
            scratch: [0; SLOT_BYTES],
        };
        for slot in (0..tx_entries).rev() {
            queues.tx_free[usize::from(queues.tx_free_len)] = slot;
            queues.tx_free_len += 1;
        }
        Ok(queues)
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn rx_queue(&self) -> &Virtqueue {
        &self.rx
    }

    pub fn tx_queue(&self) -> &Virtqueue {
        &self.tx
    }

    fn slot_ptr(&self, base: usize, slot: u16) -> *mut u8 {
        // SAFETY: callers pass `slot < entries`, so the offset is inside the
        // block (`Layout::total` covers every slot).
        unsafe { self.block.va.add(base + usize::from(slot) * SLOT_BYTES) }
    }

    fn slot_bus(&self, base: usize, slot: u16) -> u64 {
        self.block.bus + (base + usize::from(slot) * SLOT_BYTES) as u64
    }

    fn post_rx(&mut self, slot: u16) -> Result<(), Fatal> {
        let head = self.rx.add(&[Buf {
            bus: self.slot_bus(self.layout.rx_slots, slot),
            len: SLOT_BYTES as u32,
            device_writes: true,
        }])?;
        self.rx_slot[usize::from(head)] = slot;
        Ok(())
    }

    /// Give every receive slot to the device. Call once, before `DRIVER_OK`.
    pub fn post_all_rx(&mut self) -> Result<(), Fatal> {
        for slot in 0..self.layout.rx_entries {
            self.post_rx(slot)?;
        }
        Ok(())
    }

    /// Receive buffers the device currently holds.
    pub fn rx_in_flight(&self) -> u16 {
        self.rx.in_flight()
    }

    /// Hand every completed receive buffer to `f` as `(private copy, reported
    /// length)` and give the slot back to the device. The copy is
    /// `min(reported, SLOT_BYTES)` bytes, so a reported length beyond the slot
    /// shows up as `reported > copy.len()` for the caller to reject. Returns
    /// the number of completions handled, at most one full queue.
    pub fn poll_rx(&mut self, mut f: impl FnMut(&[u8], u32)) -> Result<u32, Fatal> {
        let mut handled = 0;
        while handled < u32::from(self.layout.rx_entries) {
            let Some(used) = self.rx.pop_used()? else {
                break;
            };
            // `pop_used` only returns heads that were in flight, and every
            // in-flight head was recorded by `post_rx`; anything else is a
            // bug in the driver or the queue, reported rather than indexed.
            let slot = core::mem::replace(&mut self.rx_slot[usize::from(used.head)], NONE);
            if slot >= self.layout.rx_entries {
                return Err(Fatal::Device(virtio::Error::DeviceError));
            }
            let copy_len = (used.len as usize).min(SLOT_BYTES);
            // SAFETY: `copy_len <= SLOT_BYTES`, the slot is inside the block,
            // and `scratch` is private memory; the device may still be writing
            // the slot, which yields a garbled copy, never an out-of-bounds read.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.slot_ptr(self.layout.rx_slots, slot),
                    self.scratch.as_mut_ptr(),
                    copy_len,
                );
            }
            f(&self.scratch[..copy_len], used.len);
            self.post_rx(slot)?;
            handled += 1;
        }
        Ok(handled)
    }

    /// Free transmit slots.
    pub fn tx_free(&self) -> u16 {
        self.tx_free_len
    }

    /// Transmit slots the device holds.
    pub fn tx_in_flight(&self) -> u16 {
        self.tx.in_flight()
    }

    /// Queue `frame` (the packet header is added here). The frame is copied
    /// into a driver-owned slot and never truncated.
    pub fn tx_send(&mut self, frame: &[u8]) -> Result<(), TxError> {
        if frame.len() > SLOT_BYTES - HDR_LEN {
            return Err(TxError::TooLong);
        }
        if self.tx_free_len == 0 {
            return Err(TxError::NoSlot);
        }
        let slot = self.tx_free[usize::from(self.tx_free_len) - 1];
        let dest = self.slot_ptr(self.layout.tx_slots, slot);
        // SAFETY: `HDR_LEN + frame.len() <= SLOT_BYTES`, so both copies stay
        // inside the slot, which the device is not reading (it is free).
        unsafe {
            ptr::copy_nonoverlapping(NetHdr::PLAIN.encode().as_ptr(), dest, HDR_LEN);
            ptr::copy_nonoverlapping(frame.as_ptr(), dest.add(HDR_LEN), frame.len());
        }
        let added = self.tx.add(&[Buf {
            bus: self.slot_bus(self.layout.tx_slots, slot),
            len: (HDR_LEN + frame.len()) as u32,
            device_writes: false,
        }]);
        match added {
            Ok(head) => {
                self.tx_free_len -= 1;
                self.tx_slot[usize::from(head)] = slot;
                Ok(())
            }
            // A queue as big as the pool cannot be full while a slot is free.
            Err(_) => Err(TxError::NoSlot),
        }
    }

    /// Take back transmit slots the device has finished with; returns how many.
    pub fn reap_tx(&mut self) -> Result<u16, Fatal> {
        let mut reaped = 0;
        while let Some(used) = self.tx.pop_used()? {
            let slot = core::mem::replace(&mut self.tx_slot[usize::from(used.head)], NONE);
            if slot >= self.layout.tx_entries || usize::from(self.tx_free_len) >= MAX_QUEUE {
                return Err(Fatal::Device(virtio::Error::DeviceError));
            }
            self.tx_free[usize::from(self.tx_free_len)] = slot;
            self.tx_free_len += 1;
            reaped += 1;
        }
        Ok(reaped)
    }
}

impl NicRings for Queues {
    fn poll_frames(
        &mut self,
        max_frame: usize,
        mut deliver: impl FnMut(Result<&[u8], RxError>),
    ) -> Result<u32, Fatal> {
        self.poll_rx(|buf, written| deliver(virtio_net::frame::rx_frame(buf, written, max_frame)))
    }

    fn tx_free(&self) -> u16 {
        Queues::tx_free(self)
    }

    fn tx_send(&mut self, frame: &[u8]) -> Result<(), TxError> {
        Queues::tx_send(self, frame)
    }

    fn reap_tx(&mut self) -> Result<u16, Fatal> {
        Queues::reap_tx(self)
    }

    fn rx_in_flight(&self) -> u16 {
        Queues::rx_in_flight(self)
    }
}
