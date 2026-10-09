//! The receive and transmit descriptor rings over one DMA block, as the
//! [`nicdrv`] engine's [`NicRings`].
//!
//! Every descriptor owns one fixed 2048-byte slot (descriptor `i`, slot `i`),
//! so the device only ever sees driver-owned memory at driver-chosen
//! addresses. What the device writes back (length, error bits, segment flags)
//! is untrusted: a length past the slot, a frame spread over several
//! descriptors or a reported error is dropped and counted, never used to
//! index, and a received frame is copied out of its slot before it is
//! examined.
//!
//! **Receive.** At start every descriptor is posted to the device with `OWN`
//! (the last also with `EOR`). A completion is a descriptor whose `OWN` the
//! device cleared; the descriptor goes straight back with a fresh `OWN` once
//! its frame is copied out. The chip leaves the 4-byte FCS on every frame and
//! counts it in the length, so the driver trims it before the frame leaves
//! this module: the engine's `max_frame` is the MTU plus the Ethernet header
//! and a full-size frame with its FCS would exceed it.
//!
//! **Transmit.** The driver fills the descriptor at its tail, hands it over
//! with `OWN` and rings `TxPoll`; it reaps in order from its clean index while
//! the device has cleared `OWN`. At most `entries - 1` frames are in flight.
//! Frames shorter than the Ethernet minimum are zero-padded in the slot (the
//! chip appends the FCS itself), so no revision-specific padding behaviour is
//! relied on.
//!
//! **Hostile device.** Completions must come in ring order. The device
//! completing a descriptor *behind* one it still owns is not something an
//! honest chip does; seen on two polls running (one could be a read racing
//! the chip's own write-back) it is [`Fatal::Hardware`], and nothing the
//! device writes is ever used as an index.

use core::ptr;
use core::sync::atomic::{fence, Ordering};

use nicdrv::{DmaBlock, Fatal, NicRings, RxError, TxError};

use crate::desc::{self, rx_err, DESC_BYTES, FCS_BYTES, FS, LS, OWN, RX_LEN_MASK};
use crate::regs::TX_POLL_NPQ;
use crate::regs::{cmd, cplus, rx_config, tx_config, Regs};
use crate::regs::{CHIP_CMD, CPLUS_CMD, MAR0, MAX_TX_PACKET, MAX_TX_UNITS, RDSAR_HI, RDSAR_LO};
use crate::regs::{RX_CONFIG, RX_MAX_FRAME, RX_MAX_SIZE, TNPDS_HI, TNPDS_LO, TX_CONFIG, TX_POLL};
use crate::setup;

/// Bytes per packet slot; also the size each receive descriptor offers.
pub const SLOT_BYTES: usize = 2048;
/// Ring sizes the driver accepts: powers of two (rings of 256-byte-aligned
/// pages; the chip takes up to 1024 *to confirm*, 256 is what v1 uses).
pub const MIN_ENTRIES: u16 = 8;
pub const MAX_ENTRIES: u16 = 256;
/// The Ethernet header, the shortest frame the engine accepts.
const ETH_HEADER: usize = 14;
/// The shortest frame on the wire without its FCS; shorter ones are padded.
const ETH_MIN: usize = 60;
const PAGE: usize = 4096;
/// Polls in a row an out-of-order completion must be seen before it is fatal.
const SUSPECT_LIMIT: u8 = 2;

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
    /// Consecutive polls that saw a completion behind a still-owned descriptor.
    rx_suspect: u8,
    /// Next transmit descriptor to fill, and next to reap.
    tx_tail: u16,
    tx_clean: u16,
    tx_in_flight: u16,
    tx_suspect: u8,
    /// Transmit completions since the start, for the watchdog.
    tx_reaped: u64,
    /// Private copy of the receive slot being examined.
    scratch: [u8; SLOT_BYTES],
}

impl<R: Regs> Rings<R> {
    /// Lay the rings out in `block`, post every receive buffer, program the
    /// chip's ring and configuration registers and enable the receiver and
    /// transmitter. The chip must have been reset ([`setup::reset`]).
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
            rx_suspect: 0,
            tx_tail: 0,
            tx_clean: 0,
            tx_in_flight: 0,
            tx_suspect: 0,
            tx_reaped: 0,
            scratch: [0; SLOT_BYTES],
        };
        for index in 0..rx_entries {
            rings.post_rx(index);
        }
        fence(Ordering::Release);
        rings.program();
        Ok(rings)
    }

    /// Point the chip at the rings, set the receive and transmit
    /// configuration and switch both units on.
    fn program(&mut self) {
        let base = |offset: usize| self.block.bus() + offset as u64;
        let (rx_base, tx_base) = (base(self.layout.rx_ring), base(self.layout.tx_ring));
        let regs = &mut self.regs;
        setup::unlock_config(regs);
        // No receive checksum offload and no VLAN stripping: the frame the
        // chip wrote is the frame on the wire.
        let cplus = regs.read16(CPLUS_CMD) & !(cplus::RX_VLAN | cplus::RX_CHECKSUM);
        regs.write16(CPLUS_CMD, cplus);
        regs.write16(RX_MAX_SIZE, RX_MAX_FRAME);
        regs.write8(MAX_TX_PACKET, MAX_TX_UNITS);
        regs.write32(TNPDS_HI, (tx_base >> 32) as u32);
        regs.write32(TNPDS_LO, tx_base as u32);
        regs.write32(RDSAR_HI, (rx_base >> 32) as u32);
        regs.write32(RDSAR_LO, rx_base as u32);
        regs.write32(TX_CONFIG, tx_config::DMA_BURST | tx_config::INTER_FRAME_GAP);
        // The engine filters by MAC and receive mode itself, so the chip
        // accepts every unicast and multicast frame (and broadcast) rather
        // than being reprogrammed per mode.
        regs.write32(MAR0, u32::MAX);
        regs.write32(MAR0 + 4, u32::MAX);
        regs.write32(
            RX_CONFIG,
            rx_config::ACCEPT_ALL_PHYS
                | rx_config::ACCEPT_MY_PHYS
                | rx_config::ACCEPT_MULTICAST
                | rx_config::ACCEPT_BROADCAST
                | rx_config::DMA_BURST
                | rx_config::FIFO_THRESHOLD,
        );
        setup::lock_config(regs);
        regs.write8(CHIP_CMD, cmd::RX_ENABLE | cmd::TX_ENABLE);
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

    /// Frames queued for transmission and not yet completed.
    pub fn tx_in_flight(&self) -> u16 {
        self.tx_in_flight
    }

    /// Transmit completions since the start (for [`crate::TxWatchdog`]).
    pub fn tx_reaped_total(&self) -> u64 {
        self.tx_reaped
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

    fn rx_slot_offset(&self, index: u16) -> usize {
        self.layout.rx_slots + usize::from(index) * SLOT_BYTES
    }

    /// Give receive descriptor `index` (and its slot) to the device.
    fn post_rx(&self, index: u16) {
        let bus = self.block.bus() + self.rx_slot_offset(index) as u64;
        let last = index + 1 == self.layout.rx_entries;
        // SAFETY: `index < rx_entries`; either the device is not running yet
        // or it handed this descriptor back (`OWN` clear).
        unsafe { desc::rx_post(self.rx_desc(index), bus, SLOT_BYTES as u32, last) };
    }

    fn tx_slot(&self, index: u16) -> (*mut u8, u64) {
        let offset = self.layout.tx_slots + usize::from(index) * SLOT_BYTES;
        (self.at(offset), self.block.bus() + offset as u64)
    }

    /// Judge the completed receive descriptor `index` (its `opts1` is
    /// `opts1`) and copy its frame, FCS trimmed, into `scratch`; the frame's
    /// length on success.
    fn take_rx(&mut self, index: u16, opts1: u32, max_frame: usize) -> Result<usize, RxError> {
        let whole = FS | LS;
        if opts1 & whole != whole {
            // Part of a frame spread over descriptors (the chip was told
            // 1528-byte frames and the slots hold 2048, so an honest chip
            // never does this): this and every descriptor up to the last
            // piece are dropped.
            self.rx_discarding = opts1 & LS == 0;
            return Err(RxError::Overrun);
        }
        if core::mem::replace(&mut self.rx_discarding, false) {
            return Err(RxError::Overrun);
        }
        if opts1 & rx_err::RUNT != 0 {
            return Err(RxError::Runt);
        }
        if opts1 & rx_err::ANY != 0 {
            return Err(RxError::NotPlain);
        }
        let len = (opts1 & RX_LEN_MASK) as usize;
        if len > SLOT_BYTES {
            return Err(RxError::Overrun);
        }
        // The descriptor length counts the FCS. A length that cannot even
        // hold one is a runt; trim it off before anything else sees the frame.
        let Some(frame) = len.checked_sub(FCS_BYTES) else {
            return Err(RxError::Runt);
        };
        let slot = self.at(self.rx_slot_offset(index));
        // SAFETY: `frame <= SLOT_BYTES`, the slot is inside the block and the
        // scratch buffer is private. The device may still be writing the slot
        // (a lying `OWN`), which garbles the copy but never reads out of bounds.
        unsafe { ptr::copy_nonoverlapping(slot, self.scratch.as_mut_ptr(), frame) };
        if frame < ETH_HEADER {
            Err(RxError::Runt)
        } else if frame > max_frame {
            Err(RxError::Oversize)
        } else {
            Ok(frame)
        }
    }

    /// Whether the receive descriptor after `index` is complete while `index`
    /// is still the device's: a completion out of ring order.
    fn rx_out_of_order(&self, index: u16) -> bool {
        let next = (index + 1) % self.layout.rx_entries;
        // SAFETY: `next < rx_entries`.
        let opts1 = unsafe { desc::opts1(self.rx_desc(next)) };
        opts1 & OWN == 0
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
        while handled < u32::from(entries) {
            let index = self.rx_next;
            // SAFETY: `index < entries`.
            let opts1 = unsafe { desc::opts1(self.rx_desc(index)) };
            if opts1 & OWN != 0 {
                break;
            }
            // The write-back is complete before the bytes it describes are read.
            fence(Ordering::Acquire);
            match self.take_rx(index, opts1, max_frame) {
                Ok(len) => deliver(Ok(&self.scratch[..len])),
                Err(error) => deliver(Err(error)),
            }
            self.post_rx(index);
            self.rx_next = (index + 1) % entries;
            handled += 1;
        }
        if handled < u32::from(entries) && self.rx_out_of_order(self.rx_next) {
            self.rx_suspect += 1;
            if self.rx_suspect >= SUSPECT_LIMIT {
                return Err(Fatal::Hardware("rx completion out of order"));
            }
        } else {
            self.rx_suspect = 0;
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
        let len = frame.len().max(ETH_MIN);
        let last = index + 1 == self.layout.tx_entries;
        // SAFETY: the frame (padded) fits the slot, which the device is not
        // reading (its descriptor is not in flight).
        unsafe {
            ptr::copy_nonoverlapping(frame.as_ptr(), slot, frame.len());
            ptr::write_bytes(slot.add(frame.len()), 0, len - frame.len());
            desc::tx_post(self.tx_desc(index), bus, len as u16, last);
        }
        self.tx_tail = (index + 1) % self.layout.tx_entries;
        self.tx_in_flight += 1;
        fence(Ordering::Release);
        self.regs.write8(TX_POLL, TX_POLL_NPQ);
        Ok(())
    }

    fn reap_tx(&mut self) -> Result<u16, Fatal> {
        let entries = self.layout.tx_entries;
        let mut reaped = 0;
        while self.tx_in_flight > 0 {
            // SAFETY: `tx_clean < tx_entries`.
            let opts1 = unsafe { desc::opts1(self.tx_desc(self.tx_clean)) };
            if opts1 & OWN != 0 {
                break;
            }
            self.tx_clean = (self.tx_clean + 1) % entries;
            self.tx_in_flight -= 1;
            self.tx_reaped += 1;
            reaped += 1;
        }
        // Blocked at the head: the next frame in flight must not be done.
        let behind = self.tx_in_flight > 1 && {
            let next = (self.tx_clean + 1) % entries;
            // SAFETY: `next < entries`.
            let opts1 = unsafe { desc::opts1(self.tx_desc(next)) };
            opts1 & OWN == 0
        };
        if behind {
            self.tx_suspect += 1;
            if self.tx_suspect >= SUSPECT_LIMIT {
                return Err(Fatal::Hardware("tx completion out of order"));
            }
        } else {
            self.tx_suspect = 0;
        }
        Ok(reaped)
    }

    fn rx_in_flight(&self) -> u16 {
        // Every descriptor is the device's between polls.
        self.layout.rx_entries
    }
}
