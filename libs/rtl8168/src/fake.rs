//! A model of the RTL8168H for the host tests: a register file with the
//! side effects the driver depends on, a PHY behind `PHYAR`, the descriptor
//! rings walked the way the chip walks them (by `OWN` and `EOR`, never by a
//! length it was told), and knobs to make it lie.
//!
//! It shares the DMA block with the driver (bus addresses map to host memory
//! at a fixed offset) and checks every address the driver gives it, so a
//! driver bug that hands the chip memory outside the block fails the test.
//! Received frames carry a 4-byte FCS the way the real chip leaves it, so a
//! driver that forgets to trim it fails the test too.

use std::alloc::{alloc_zeroed, dealloc, Layout as MemLayout};
use std::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use nicdrv::DmaBlock;

use crate::desc::{EOR, FS, LS, OWN, RX_LEN_MASK, TX_LEN_MASK};
use crate::regs::*;
use crate::setup::XID_8168H;

/// Bus address the block starts at.
pub const BUS: u64 = 0x4000_0000;
/// The FCS the model appends to every received frame: a recognisable value.
pub const FCS: [u8; 4] = [0xFC, 0xFC, 0xFC, 0xFC];
/// A station address in `IDR`.
pub const MAC: [u8; 6] = [0x00, 0xE0, 0x4C, 0x12, 0x34, 0x56];

/// Host memory standing in for the DMA block.
pub struct Memory {
    pub va: *mut u8,
    pub len: usize,
}

impl Memory {
    pub fn new(len: usize) -> Memory {
        let layout = MemLayout::from_size_align(len, 4096).unwrap();
        // SAFETY: non-zero size.
        let va = unsafe { alloc_zeroed(layout) };
        assert!(!va.is_null());
        Memory { va, len }
    }

    pub fn block(&self) -> DmaBlock {
        // SAFETY: the allocation lives as long as `self`, which the tests keep
        // alive for as long as the rings.
        unsafe { DmaBlock::new(self.va, BUS, self.len) }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { dealloc(self.va, MemLayout::from_size_align(self.len, 4096).unwrap()) };
    }
}

/// `TxConfig` as a chip of revision `xid` reads it: the XID bits scattered the
/// way the real register scatters them, plus a burst setting.
pub fn tx_config_for(xid: u16) -> u32 {
    let low = u32::from(xid & 0xF) << 20;
    let high = u32::from((xid >> 6) & 0x3F) << 26;
    low | high | 0x0000_0700
}

/// A `PHYAR` access in progress: the value reads return for `busy` more
/// reads, then `done`.
struct PhyAccess {
    busy: u32,
    pending: u32,
    done: u32,
}

/// The chip's state.
pub struct Card {
    pub regs: [u8; 256],
    mem: *mut u8,
    mem_len: usize,
    /// Reads of `ChipCmd` before a reset completes.
    pub reset_reads: u32,
    resetting: bool,
    pub phy: [u16; 32],
    /// Reads of `PHYAR` before an access completes (`u32::MAX`: never).
    pub phy_busy_reads: u32,
    phy_access: Option<PhyAccess>,
    /// Pending interrupt causes (`IntrStatus`).
    pub causes: u16,
    rx_head: u32,
    tx_head: u32,
    /// Frames the chip put on the wire (no FCS, as the driver queued them).
    pub sent: Vec<Vec<u8>>,
    /// Whether a `TxPoll` write sends at once (otherwise [`Fake::complete_tx`]).
    pub auto_tx: bool,
    /// `TxPoll` writes seen.
    pub doorbells: u32,
}

/// The register handle the driver gets; the test keeps a clone to play the
/// chip.
#[derive(Clone)]
pub struct Fake(pub Rc<RefCell<Card>>);

impl Fake {
    pub fn new(memory: &Memory) -> Fake {
        Fake::with_xid(memory, XID_8168H)
    }

    pub fn with_xid(memory: &Memory, xid: u16) -> Fake {
        let mut regs = [0u8; 256];
        regs[IDR0 as usize..IDR0 as usize + 6].copy_from_slice(&MAC);
        regs[TX_CONFIG as usize..TX_CONFIG as usize + 4]
            .copy_from_slice(&tx_config_for(xid).to_le_bytes());
        // Link up at 1000 full duplex.
        regs[PHY_STATUS as usize] =
            phy_status::LINK | phy_status::SPEED_1000 | phy_status::FULL_DUPLEX;
        let mut phy = [0u16; 32];
        phy[1] = 0x796D;
        phy[2] = 0x001C;
        phy[3] = 0xC800;
        Fake(Rc::new(RefCell::new(Card {
            regs,
            mem: memory.va,
            mem_len: memory.len,
            reset_reads: 3,
            resetting: false,
            phy,
            phy_busy_reads: 2,
            phy_access: None,
            causes: 0,
            rx_head: 0,
            tx_head: 0,
            sent: Vec::new(),
            auto_tx: true,
            doorbells: 0,
        })))
    }

    pub fn reg32(&self, offset: u32) -> u32 {
        let card = self.0.borrow();
        let at = offset as usize;
        u32::from_le_bytes(card.regs[at..at + 4].try_into().unwrap())
    }

    pub fn reg16(&self, offset: u32) -> u16 {
        self.reg32(offset & !3).to_le_bytes()[(offset & 2) as usize..][..2]
            .try_into()
            .map(u16::from_le_bytes)
            .unwrap()
    }

    pub fn reg8(&self, offset: u32) -> u8 {
        self.0.borrow().regs[offset as usize]
    }

    pub fn set_reg8(&self, offset: u32, value: u8) {
        self.0.borrow_mut().regs[offset as usize] = value;
    }

    pub fn set_reg32(&self, offset: u32, value: u32) {
        let at = offset as usize;
        self.0.borrow_mut().regs[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn raise(&self, causes: u16) {
        self.0.borrow_mut().causes |= causes;
    }

    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.0.borrow().sent.clone()
    }

    /// Deliver `frame` the way the chip does: its bytes and then the FCS into
    /// the buffer of the next descriptor the driver left owned, `FS | LS`,
    /// the length counting the FCS, `OWN` cleared. A frame whose bytes and FCS
    /// do not fit the buffer is spread over descriptors (a misbehaving chip,
    /// to exercise the driver's refusal). False when the driver left no
    /// descriptor (the chip drops the frame).
    pub fn deliver(&self, frame: &[u8]) -> bool {
        let mut wire = frame.to_vec();
        wire.extend_from_slice(&FCS);
        self.deliver_raw(&wire, 0)
    }

    /// As [`Fake::deliver`], but `wire` goes out as is (no FCS added) and
    /// `errors` is ORed into the descriptor's status bits.
    pub fn deliver_raw(&self, wire: &[u8], errors: u32) -> bool {
        let mut card = self.0.borrow_mut();
        // Walk the descriptors this would take without committing: all of
        // them must be the chip's to write.
        let mut head = card.rx_head;
        let mut plan = Vec::new();
        let mut rest = wire;
        loop {
            let desc = card.rx_desc(head);
            let opts1 = card.read_u32(desc);
            if opts1 & OWN == 0 {
                return false;
            }
            let size = (opts1 & RX_LEN_MASK) as usize;
            let take = rest.len().min(size);
            plan.push((head, desc, opts1, take));
            rest = &rest[take..];
            head = if opts1 & EOR != 0 { 0 } else { head + 1 };
            if rest.is_empty() {
                break;
            }
        }
        let count = plan.len();
        let mut offset = 0;
        for (index, (_, desc, opts1, take)) in plan.into_iter().enumerate() {
            let addr = card.read_u64(desc + 8);
            card.write_bytes(addr, &wire[offset..offset + take]);
            offset += take;
            let mut flags = 0;
            if index == 0 {
                flags |= FS;
            }
            if index + 1 == count {
                flags |= LS;
            }
            let done = (opts1 & EOR) | flags | errors | take as u32;
            card.write_u32(desc, done);
        }
        card.rx_head = head;
        true
    }

    /// Write `opts1` into receive descriptor `index` regardless of who owns
    /// it, as a broken or malicious chip would.
    pub fn lie_rx(&self, index: u32, opts1: u32) {
        let mut card = self.0.borrow_mut();
        let entries = card.rx_entries();
        let desc = card.rx_desc(index % entries);
        // A lie is about a completion; where the ring ends is the driver's
        // data, which the model (like the chip) walks by.
        let eor = card.read_u32(desc) & EOR;
        card.write_u32(desc, (opts1 & !EOR) | eor);
    }

    /// Clear `OWN` on transmit descriptor `index` whether or not it was sent.
    pub fn lie_tx_done(&self, index: u32) {
        let mut card = self.0.borrow_mut();
        let entries = card.tx_entries();
        let desc = card.tx_desc(index % entries);
        let opts1 = card.read_u32(desc);
        card.write_u32(desc, opts1 & !OWN);
    }

    /// Send whatever the driver queued (when `auto_tx` is off).
    pub fn complete_tx(&self) {
        self.0.borrow_mut().send_queued();
    }
}

impl Card {
    /// The descriptor index the model will fill next.
    pub fn rx_head_for_test(&self) -> usize {
        self.rx_head as usize
    }

    /// Move the model's head past the descriptor a test wrote by hand.
    pub fn skip_rx_for_test(&mut self) {
        let opts1 = self.read_u32(self.rx_desc(self.rx_head));
        self.rx_head = if opts1 & EOR != 0 {
            0
        } else {
            self.rx_head + 1
        };
    }

    fn ring_base(&self, low: u32, high: u32) -> u64 {
        let word = |offset: u32| {
            let at = offset as usize;
            u64::from(u32::from_le_bytes(
                self.regs[at..at + 4].try_into().unwrap(),
            ))
        };
        word(low) | word(high) << 32
    }

    fn rx_desc(&self, index: u32) -> u64 {
        self.ring_base(RDSAR_LO, RDSAR_HI) + u64::from(index) * 16
    }

    fn tx_desc(&self, index: u32) -> u64 {
        self.ring_base(TNPDS_LO, TNPDS_HI) + u64::from(index) * 16
    }

    /// Ring sizes are the driver's business; the tests that lie about a
    /// descriptor by index only need a bound, and the layout's is 256 at most.
    fn rx_entries(&self) -> u32 {
        self.ring_entries(self.rx_desc(0))
    }

    fn tx_entries(&self) -> u32 {
        self.ring_entries(self.tx_desc(0))
    }

    /// Entries up to and including the first descriptor with `EOR`.
    fn ring_entries(&self, base: u64) -> u32 {
        (0..256u32)
            .find(|index| self.read_u32(base + u64::from(*index) * 16) & EOR != 0)
            .map_or(256, |last| last + 1)
    }

    /// Host pointer for `len` bytes at bus address `addr`; panics outside the
    /// block, which is how a driver bug shows up.
    fn host(&self, addr: u64, len: usize) -> *mut u8 {
        let offset = addr.checked_sub(BUS).expect("bus address below the block") as usize;
        assert!(
            offset + len <= self.mem_len,
            "the driver gave the chip {addr:#x}+{len}, outside the block"
        );
        // SAFETY: checked inside the block.
        unsafe { self.mem.add(offset) }
    }

    fn read_u32(&self, addr: u64) -> u32 {
        // SAFETY: `host` checked the range.
        unsafe { (self.host(addr, 4) as *const u32).read_volatile() }
    }

    fn read_u64(&self, addr: u64) -> u64 {
        // SAFETY: `host` checked the range.
        unsafe { (self.host(addr, 8) as *const u64).read_unaligned() }
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        // SAFETY: `host` checked the range.
        unsafe { (self.host(addr, 4) as *mut u32).write_volatile(value) }
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        let at = self.host(addr, bytes.len());
        // SAFETY: `host` checked the range.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len()) };
    }

    /// Walk the transmit ring from the head while the driver has handed
    /// descriptors over: copy each frame out, clear `OWN`.
    fn send_queued(&mut self) {
        loop {
            let desc = self.tx_desc(self.tx_head);
            let opts1 = self.read_u32(desc);
            if opts1 & OWN == 0 {
                break;
            }
            assert_eq!(opts1 & (FS | LS), FS | LS, "one descriptor per frame");
            let len = (opts1 & TX_LEN_MASK) as usize;
            let addr = self.read_u64(desc + 8);
            let at = self.host(addr, len);
            // SAFETY: `host` checked the range.
            let frame = unsafe { core::slice::from_raw_parts(at, len) }.to_vec();
            self.sent.push(frame);
            self.write_u32(desc, opts1 & !OWN);
            self.tx_head = if opts1 & EOR != 0 {
                0
            } else {
                self.tx_head + 1
            };
        }
    }

    fn finish_reset(&mut self) {
        self.resetting = false;
        self.regs[CHIP_CMD as usize] = 0;
        self.regs[INTR_MASK as usize..INTR_MASK as usize + 2].fill(0);
        self.causes = 0;
        self.rx_head = 0;
        self.tx_head = 0;
    }

    fn phy_access(&mut self, value: u32) {
        let reg = ((value >> phyar::REG_SHIFT) & phyar::REG_MASK) as usize;
        let data = (value & phyar::DATA_MASK) as u16;
        let (pending, done) = if value & phyar::FLAG != 0 {
            self.phy[reg] = data;
            (value, value & !phyar::FLAG)
        } else {
            (value, value | phyar::FLAG | u32::from(self.phy[reg]))
        };
        self.phy_access = Some(PhyAccess {
            busy: self.phy_busy_reads,
            pending,
            done,
        });
    }
}

impl Regs for Fake {
    fn read8(&self, offset: u32) -> u8 {
        let mut card = self.0.borrow_mut();
        if offset == CHIP_CMD && card.resetting {
            if card.reset_reads > 0 {
                card.reset_reads -= 1;
            } else {
                card.finish_reset();
            }
        }
        card.regs[offset as usize]
    }

    fn read16(&self, offset: u32) -> u16 {
        let card = self.0.borrow();
        if offset == INTR_STATUS {
            return card.causes;
        }
        let at = offset as usize;
        u16::from_le_bytes(card.regs[at..at + 2].try_into().unwrap())
    }

    fn read32(&self, offset: u32) -> u32 {
        let mut card = self.0.borrow_mut();
        if offset == PHYAR {
            if let Some(access) = card.phy_access.as_mut() {
                if access.busy > 0 {
                    if access.busy != u32::MAX {
                        access.busy -= 1;
                    }
                    return access.pending;
                }
                return access.done;
            }
        }
        let at = offset as usize;
        u32::from_le_bytes(card.regs[at..at + 4].try_into().unwrap())
    }

    fn write8(&mut self, offset: u32, value: u8) {
        let mut card = self.0.borrow_mut();
        match offset {
            CHIP_CMD if value & cmd::RESET != 0 => {
                card.resetting = true;
                card.regs[CHIP_CMD as usize] = cmd::RESET;
            }
            TX_POLL => {
                card.doorbells += 1;
                if value & TX_POLL_NPQ != 0 && card.auto_tx {
                    card.send_queued();
                }
            }
            _ => card.regs[offset as usize] = value,
        }
    }

    fn write16(&mut self, offset: u32, value: u16) {
        let mut card = self.0.borrow_mut();
        if offset == INTR_STATUS {
            // Write one to clear.
            card.causes &= !value;
            return;
        }
        let at = offset as usize;
        card.regs[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write32(&mut self, offset: u32, value: u32) {
        let mut card = self.0.borrow_mut();
        if offset == PHYAR {
            card.phy_access(value);
            return;
        }
        let at = offset as usize;
        card.regs[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}
