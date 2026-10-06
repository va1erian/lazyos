//! The receive and transmit descriptor rings over one DMA block, as the
//! [`nicdrv`] engine's [`NicRings`].
//!
//! Every descriptor owns one fixed 2048-byte slot (descriptor `i`, slot `i`),
//! so the device only ever sees driver-owned memory at driver-chosen
//! addresses. What the device writes back (length, status, errors) is
//! untrusted: a length past the slot, a frame spread over several descriptors
//! or a reported error is dropped and counted, never used to index, and a
//! received frame is copied out of its slot before it is examined.
//!
//! **Receive.** At start every descriptor holds a buffer and the tail sits one
//! behind the head, so the device owns all but one ("the gap"). A completed
//! descriptor is handed back with a fresh status and becomes the new tail, so
//! the gap moves along the ring.
//!
//! **Transmit.** The driver fills the descriptor at its tail and moves the
//! tail; it reaps in order from its clean index while the device has written
//! `DD` back. At most `entries - 1` frames are in flight, so a full ring is
//! never mistaken for an empty one.

use core::ptr;
use core::sync::atomic::{fence, Ordering};

use nicdrv::{DmaBlock, Fatal, NicRings, RxError, TxError};

use crate::desc::{self, rx_status, DESC_BYTES, RX_ERRORS, TX_DD};
use crate::regs::{rctl, tctl, Regs, RCTL, RDBAH, RDBAL, RDH, RDLEN, RDT, TCTL, TDBAH};
use crate::regs::{TDBAL, TDH, TDLEN, TDT, TIPG, TIPG_COPPER};

/// Bytes per packet slot; `RCTL.BSIZE` 2048.
pub const SLOT_BYTES: usize = 2048;
/// Ring sizes the driver accepts: powers of two whose ring length is a
/// multiple of the 128 bytes `RDLEN`/`TDLEN` require.
pub const MIN_ENTRIES: u16 = 8;
pub const MAX_ENTRIES: u16 = 256;
/// The Ethernet header, the shortest frame the engine accepts.
const ETH_HEADER: usize = 14;
const PAGE: usize = 4096;

/// Where everything sits in the block; every part starts on a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub rx_entries: u16,
    pub tx_entries: u16,
    pub rx_ring: usize,
    pub tx_ring: usize,
    pub rx_slots: usize,
    pub tx_slots: usize,
    pub total: usize,
}

fn round_up(value: usize) -> usize {
    value.div_ceil(PAGE) * PAGE
}

impl Layout {
    /// `None` unless both sizes are powers of two in
    /// [`MIN_ENTRIES`]..=[`MAX_ENTRIES`].
    pub fn new(rx_entries: u16, tx_entries: u16) -> Option<Layout> {
        let legal = |n: u16| (MIN_ENTRIES..=MAX_ENTRIES).contains(&n) && n.is_power_of_two();
        if !legal(rx_entries) || !legal(tx_entries) {
            return None;
        }
        let rx_ring = 0;
        let tx_ring = rx_ring + round_up(usize::from(rx_entries) * DESC_BYTES);
        let rx_slots = tx_ring + round_up(usize::from(tx_entries) * DESC_BYTES);
        let tx_slots = rx_slots + usize::from(rx_entries) * SLOT_BYTES;
        let total = tx_slots + usize::from(tx_entries) * SLOT_BYTES;
        Some(Layout {
            rx_entries,
            tx_entries,
            rx_ring,
            tx_ring,
            rx_slots,
            tx_slots,
            total,
        })
    }
}

/// The rings, the device's registers and the slot memory.
pub struct Rings<R: Regs> {
    regs: R,
    block: DmaBlock,
    layout: Layout,
    /// Next receive descriptor to look at.
    rx_next: u16,
    /// Dropping the rest of a frame the device spread over descriptors.
    rx_discarding: bool,
    /// Next transmit descriptor to fill, and next to reap.
    tx_tail: u16,
    tx_clean: u16,
    tx_in_flight: u16,
    /// Private copy of the receive slot being examined.
    scratch: [u8; SLOT_BYTES],
}

impl<R: Regs> Rings<R> {
    /// Lay the rings out in `block`, post every receive buffer, program the
    /// ring registers and enable the receiver and transmitter.
    ///
    /// # Safety
    /// As [`DmaBlock::new`]: the block must outlive the rings and be
    /// exclusively theirs, and `regs` must be the device the block's bus
    /// addresses were allocated for.
    pub unsafe fn new(
        regs: R,
        block: DmaBlock,
        rx_entries: u16,
        tx_entries: u16,
    ) -> Result<Rings<R>, Fatal> {
        let layout = Layout::new(rx_entries, tx_entries).ok_or(Fatal::Layout)?;
        if block.len() < layout.total || !(block.va() as usize).is_multiple_of(PAGE) {
            return Err(Fatal::Layout);
        }
        // SAFETY: the descriptor rings lie inside the block (`total` fits).
        unsafe { ptr::write_bytes(block.va(), 0, layout.rx_slots) };
        let mut rings = Rings {
            regs,
            block,
            layout,
            rx_next: 0,
            rx_discarding: false,
            tx_tail: 0,
            tx_clean: 0,
            tx_in_flight: 0,
            scratch: [0; SLOT_BYTES],
        };
        for index in 0..rx_entries {
            let bus = rings.rx_bus(index);
            // SAFETY: `index < rx_entries`; the device is not running yet.
            unsafe { desc::rx_post(rings.rx_desc(index), bus) };
        }
        fence(Ordering::Release);
        rings.program();
        Ok(rings)
    }

    /// Point the device at the rings and switch both units on.
    fn program(&mut self) {
        let base = |offset: usize| self.block.bus() + offset as u64;
        let (rx_base, tx_base) = (base(self.layout.rx_ring), base(self.layout.tx_ring));
        let (rx_entries, tx_entries) = (self.layout.rx_entries, self.layout.tx_entries);
        let regs = &mut self.regs;
        regs.write(RDBAL, rx_base as u32);
        regs.write(RDBAH, (rx_base >> 32) as u32);
        regs.write(RDLEN, u32::from(rx_entries) * DESC_BYTES as u32);
        regs.write(RDH, 0);
        regs.write(RDT, u32::from(rx_entries - 1));
        regs.write(TDBAL, tx_base as u32);
        regs.write(TDBAH, (tx_base >> 32) as u32);
        regs.write(TDLEN, u32::from(tx_entries) * DESC_BYTES as u32);
        regs.write(TDH, 0);
        regs.write(TDT, 0);
        regs.write(TIPG, TIPG_COPPER);
        // The engine filters by MAC and receive mode itself, so the card
        // accepts everything rather than being reprogrammed per mode.
        regs.write(
            RCTL,
            rctl::EN | rctl::UPE | rctl::MPE | rctl::BAM | rctl::SECRC,
        );
        regs.write(TCTL, tctl::EN | tctl::PSP | tctl::CT | tctl::COLD);
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The device's registers (link state, interrupt causes).
    pub fn regs(&self) -> &R {
        &self.regs
    }

    pub fn regs_mut(&mut self) -> &mut R {
        &mut self.regs
    }

    fn at(&self, offset: usize) -> *mut u8 {
        // SAFETY: callers compute offsets from the layout with an index below
        // the ring size, so the result is inside the block.
        unsafe { self.block.va().add(offset) }
    }

    fn rx_desc(&self, index: u16) -> *mut u8 {
        self.at(self.layout.rx_ring + usize::from(index) * DESC_BYTES)
    }

    fn tx_desc(&self, index: u16) -> *mut u8 {
        self.at(self.layout.tx_ring + usize::from(index) * DESC_BYTES)
    }

    fn rx_bus(&self, index: u16) -> u64 {
        self.block.bus() + (self.layout.rx_slots + usize::from(index) * SLOT_BYTES) as u64
    }

    fn tx_slot(&self, index: u16) -> (*mut u8, u64) {
        let offset = self.layout.tx_slots + usize::from(index) * SLOT_BYTES;
        (self.at(offset), self.block.bus() + offset as u64)
    }

    /// Judge the completed receive descriptor `index` and copy its frame into
    /// `scratch`; the frame's length on success.
    fn take_rx(&mut self, index: u16, max_frame: usize) -> Result<usize, RxError> {
        // SAFETY: `index < rx_entries`.
        let done = unsafe { desc::rx_read(self.rx_desc(index)) };
        if done.status & rx_status::EOP == 0 {
            // A frame longer than a slot: this and every descriptor up to its
            // end are dropped (the card was told 2048-byte buffers, and no
            // MTU the driver allows needs more).
            self.rx_discarding = true;
            return Err(RxError::Overrun);
        }
        if core::mem::replace(&mut self.rx_discarding, false) {
            return Err(RxError::Overrun);
        }
        if done.errors & RX_ERRORS != 0 {
            return Err(RxError::NotPlain);
        }
        let len = usize::from(done.length);
        if len > SLOT_BYTES {
            return Err(RxError::Overrun);
        }
        let slot = self.at(self.layout.rx_slots + usize::from(index) * SLOT_BYTES);
        // SAFETY: `len <= SLOT_BYTES`, the slot is inside the block and the
        // scratch buffer is private. The device may still be writing the slot
        // (a lying `DD`), which garbles the copy but never reads out of bounds.
        unsafe { ptr::copy_nonoverlapping(slot, self.scratch.as_mut_ptr(), len) };
        if len < ETH_HEADER {
            Err(RxError::Runt)
        } else if len > max_frame {
            Err(RxError::Oversize)
        } else {
            Ok(len)
        }
    }
}

impl<R: Regs> NicRings for Rings<R> {
    fn poll_frames(
        &mut self,
        max_frame: usize,
        mut deliver: impl FnMut(Result<&[u8], RxError>),
    ) -> Result<u32, Fatal> {
        let entries = self.layout.rx_entries;
        let mut handled = 0u32;
        let mut tail = None;
        while handled < u32::from(entries) {
            let index = self.rx_next;
            // SAFETY: `index < entries`.
            let status = unsafe { desc::rx_read(self.rx_desc(index)) }.status;
            if status & rx_status::DD == 0 {
                break;
            }
            // The write-back is complete before the bytes it describes are read.
            fence(Ordering::Acquire);
            match self.take_rx(index, max_frame) {
                Ok(len) => deliver(Ok(&self.scratch[..len])),
                Err(error) => deliver(Err(error)),
            }
            let bus = self.rx_bus(index);
            // SAFETY: the device handed this descriptor back (`DD`), and the
            // tail below is what returns it.
            unsafe { desc::rx_post(self.rx_desc(index), bus) };
            tail = Some(index);
            self.rx_next = (index + 1) % entries;
            handled += 1;
        }
        if let Some(tail) = tail {
            // The re-posted descriptors are written before the device can see
            // them through the tail.
            fence(Ordering::Release);
            self.regs.write(RDT, u32::from(tail));
        }
        Ok(handled)
    }

    fn tx_free(&self) -> u16 {
        self.layout.tx_entries - 1 - self.tx_in_flight
    }

    fn tx_send(&mut self, frame: &[u8]) -> Result<(), TxError> {
        if frame.len() > SLOT_BYTES {
            return Err(TxError::TooLong);
        }
        if self.tx_free() == 0 {
            return Err(TxError::NoSlot);
        }
        let index = self.tx_tail;
        let (slot, bus) = self.tx_slot(index);
        // SAFETY: the frame fits the slot, which the device is not reading
        // (its descriptor is not in flight).
        unsafe {
            ptr::copy_nonoverlapping(frame.as_ptr(), slot, frame.len());
            desc::tx_post(self.tx_desc(index), bus, frame.len() as u16);
        }
        self.tx_tail = (index + 1) % self.layout.tx_entries;
        self.tx_in_flight += 1;
        fence(Ordering::Release);
        self.regs.write(TDT, u32::from(self.tx_tail));
        Ok(())
    }

    fn reap_tx(&mut self) -> Result<u16, Fatal> {
        let mut reaped = 0;
        while self.tx_in_flight > 0 {
            // SAFETY: `tx_clean < tx_entries`.
            let status = unsafe { desc::tx_status(self.tx_desc(self.tx_clean)) };
            if status & TX_DD == 0 {
                break;
            }
            self.tx_clean = (self.tx_clean + 1) % self.layout.tx_entries;
            self.tx_in_flight -= 1;
            reaped += 1;
        }
        Ok(reaped)
    }

    fn rx_in_flight(&self) -> u16 {
        // Every descriptor but the gap is the device's.
        self.layout.rx_entries - 1
    }
}
