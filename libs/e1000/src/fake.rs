//! A model of an 8254x for the host tests: a register file, the descriptor
//! rings walked the way the card walks them, and knobs to make it lie.
//!
//! It shares the DMA block with the driver (bus addresses map to host
//! memory at a fixed offset) and checks every address the driver gives it, so
//! a driver bug that hands the card memory outside the block fails the test.

use std::alloc::{alloc_zeroed, dealloc, Layout as MemLayout};
use std::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use nicdrv::DmaBlock;

use crate::desc::{rx_status, DESC_BYTES, TX_DD};
use crate::regs::*;

/// Bus address the block starts at.
pub const BUS: u64 = 0x4000_0000;

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

/// The card's state.
pub struct Card {
    pub regs: Vec<u32>,
    mem: *mut u8,
    mem_len: usize,
    /// Reads of `CTRL` before a reset completes.
    pub reset_reads: u32,
    pub eeprom: [u16; 3],
    pub icr: u32,
    /// Frames the card put on the wire.
    pub sent: Vec<Vec<u8>>,
    /// Whether a `TDT` write sends at once (otherwise [`Fake::complete_tx`]).
    pub auto_tx: bool,
}

/// The register handle the driver gets; the test keeps a clone to play the
/// card.
#[derive(Clone)]
pub struct Fake(pub Rc<RefCell<Card>>);

impl Fake {
    pub fn new(memory: &Memory) -> Fake {
        let mut regs = std::vec![0u32; (MIN_BAR_BYTES / 4) as usize];
        // An EEPROM-loaded station address 52:54:00:12:34:56, link up.
        regs[(RAL0 / 4) as usize] = u32::from_le_bytes([0x52, 0x54, 0x00, 0x12]);
        regs[(RAH0 / 4) as usize] = u32::from(u16::from_le_bytes([0x34, 0x56])) | RAH_AV;
        regs[(STATUS / 4) as usize] = status::LU;
        Fake(Rc::new(RefCell::new(Card {
            regs,
            mem: memory.va,
            mem_len: memory.len,
            reset_reads: 3,
            eeprom: [0x5452, 0x1200, 0x5634],
            icr: 0,
            sent: Vec::new(),
            auto_tx: true,
        })))
    }

    pub fn reg(&self, offset: u32) -> u32 {
        self.0.borrow().regs[(offset / 4) as usize]
    }

    pub fn set_reg(&self, offset: u32, value: u32) {
        self.0.borrow_mut().regs[(offset / 4) as usize] = value;
    }

    pub fn raise(&self, causes: u32) {
        self.0.borrow_mut().icr |= causes;
    }

    /// Deliver `frame` the way the card does: into the buffers from the head,
    /// one descriptor per 2048 bytes, `EOP` on the last. False when the
    /// driver left no descriptor (the card drops it).
    pub fn deliver(&self, frame: &[u8]) -> bool {
        let mut card = self.0.borrow_mut();
        let pieces: Vec<&[u8]> = if frame.is_empty() {
            std::vec![frame]
        } else {
            frame.chunks(2048).collect()
        };
        let free = card.rx_free();
        if free < pieces.len() {
            return false;
        }
        for (index, piece) in pieces.iter().enumerate() {
            let head = card.regs[(RDH / 4) as usize];
            let desc = card.rx_desc(head);
            let addr = card.read_u64(desc);
            card.write_bytes(addr, piece);
            let eop = if index + 1 == pieces.len() {
                rx_status::EOP
            } else {
                0
            };
            card.write_desc_fields(desc, piece.len() as u16, rx_status::DD | eop, 0);
            card.advance(RDH, RDLEN);
        }
        true
    }

    /// Write a completion into receive descriptor `index` regardless of who
    /// owns it, as a broken or malicious card would.
    pub fn lie_rx(&self, index: u32, length: u16, status: u8, errors: u8) {
        let mut card = self.0.borrow_mut();
        let desc = card.rx_desc(index % card.entries(RDLEN));
        card.write_desc_fields(desc, length, status, errors);
    }

    /// Mark transmit descriptor `index` done whether or not it was sent.
    pub fn lie_tx_done(&self, index: u32) {
        let mut card = self.0.borrow_mut();
        let desc = card.tx_desc(index % card.entries(TDLEN));
        card.write_byte(desc + 12, TX_DD);
    }

    /// Send whatever the driver queued (when `auto_tx` is off).
    pub fn complete_tx(&self) {
        self.0.borrow_mut().send_queued();
    }
}

impl Card {
    fn entries(&self, len_reg: u32) -> u32 {
        (self.regs[(len_reg / 4) as usize] / DESC_BYTES as u32).max(1)
    }

    fn base(&self, low: u32) -> u64 {
        u64::from(self.regs[(low / 4) as usize])
            | u64::from(self.regs[(low / 4 + 1) as usize]) << 32
    }

    fn rx_desc(&self, index: u32) -> u64 {
        self.base(RDBAL) + u64::from(index) * DESC_BYTES as u64
    }

    fn tx_desc(&self, index: u32) -> u64 {
        self.base(TDBAL) + u64::from(index) * DESC_BYTES as u64
    }

    /// Descriptors the driver gave the card and it has not filled.
    fn rx_free(&self) -> usize {
        let n = self.entries(RDLEN);
        let (head, tail) = (self.regs[(RDH / 4) as usize], self.regs[(RDT / 4) as usize]);
        ((tail + n - head) % n) as usize
    }

    fn advance(&mut self, reg: u32, len_reg: u32) {
        let n = self.entries(len_reg);
        let at = (reg / 4) as usize;
        self.regs[at] = (self.regs[at] + 1) % n;
    }

    /// Host pointer for `len` bytes at bus address `addr`; panics outside the
    /// block, which is how a driver bug shows up.
    fn host(&self, addr: u64, len: usize) -> *mut u8 {
        let offset = addr.checked_sub(BUS).expect("bus address below the block") as usize;
        assert!(
            offset + len <= self.mem_len,
            "the driver gave the card {addr:#x}+{len}, outside the block"
        );
        // SAFETY: checked inside the block.
        unsafe { self.mem.add(offset) }
    }

    fn read_u64(&self, addr: u64) -> u64 {
        // SAFETY: `host` checked the range.
        unsafe { (self.host(addr, 8) as *const u64).read_unaligned() }
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        let at = self.host(addr, bytes.len());
        // SAFETY: `host` checked the range.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len()) };
    }

    fn write_byte(&mut self, addr: u64, value: u8) {
        self.write_bytes(addr, &[value]);
    }

    fn write_desc_fields(&mut self, desc: u64, length: u16, status: u8, errors: u8) {
        let len = length.to_le_bytes();
        self.write_bytes(desc + 8, &[len[0], len[1], 0, 0, status, errors]);
    }

    /// Walk the transmit ring from the head to the tail: copy each frame out,
    /// write `DD` back.
    fn send_queued(&mut self) {
        let n = self.entries(TDLEN);
        while self.regs[(TDH / 4) as usize] != self.regs[(TDT / 4) as usize] % n {
            let desc = self.tx_desc(self.regs[(TDH / 4) as usize]);
            let addr = self.read_u64(desc);
            // SAFETY: inside the block.
            let len = unsafe { (self.host(desc + 8, 2) as *const u16).read_unaligned() } as usize;
            let at = self.host(addr, len);
            // SAFETY: `host` checked the range.
            let frame = unsafe { core::slice::from_raw_parts(at, len) }.to_vec();
            self.sent.push(frame);
            self.write_byte(desc + 12, TX_DD);
            self.advance(TDH, TDLEN);
        }
    }
}

impl Regs for Fake {
    fn read(&self, offset: u32) -> u32 {
        let mut card = self.0.borrow_mut();
        match offset {
            CTRL if card.regs[0] & ctrl::RST != 0 => {
                if card.reset_reads > 0 {
                    card.reset_reads -= 1;
                } else {
                    card.regs[0] &= !ctrl::RST;
                }
                card.regs[0]
            }
            ICR => core::mem::take(&mut card.icr),
            _ => card.regs[(offset / 4) as usize],
        }
    }

    fn write(&mut self, offset: u32, value: u32) {
        let mut card = self.0.borrow_mut();
        match offset {
            EERD if value & eerd::START != 0 => {
                let word = (value >> eerd::ADDR_SHIFT) as usize;
                let data = card.eeprom.get(word).copied().unwrap_or(0xFFFF);
                card.regs[(EERD / 4) as usize] = eerd::DONE | u32::from(data) << eerd::DATA_SHIFT;
            }
            TDT => {
                card.regs[(TDT / 4) as usize] = value;
                if card.auto_tx {
                    card.send_queued();
                }
            }
            _ => card.regs[(offset / 4) as usize] = value,
        }
    }
}
