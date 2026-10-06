//! Models for the host tests: an HDA controller (registers, the command rings
//! walked the way the hardware walks them, one output stream whose position
//! the test moves) and codecs built from a node table.

use std::alloc::{alloc_zeroed, dealloc, Layout as MemLayout};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use crate::controller::RingMemory;
use crate::regs::*;
use crate::verbs::{param, GET_CONFIG_DEFAULT, GET_CONN_LIST, GET_PARAMETER};

pub const BUS: u64 = 0x5000_0000;
/// Input and output stream counts (QEMU's ICH6 model has four of each).
pub const INPUTS: u32 = 4;
pub const OUTPUTS: u32 = 4;
/// The first output stream descriptor.
pub const OUT0: u32 = SD_BASE + SD_STRIDE * INPUTS;

/// Host memory standing in for DMA memory.
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

    pub fn rings(&self) -> RingMemory {
        RingMemory {
            va: self.va,
            bus: BUS,
        }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { dealloc(self.va, MemLayout::from_size_align(self.len, 4096).unwrap()) };
    }
}

/// One codec node: its parameters, connections and pin configuration.
#[derive(Clone, Default)]
pub struct Node {
    pub params: BTreeMap<u8, u32>,
    /// The raw connection-list entries as the codec reports them.
    pub conn_raw: Vec<u16>,
    pub long_form: bool,
    pub config: u32,
}

/// A codec: a node table, and every set-verb it received.
#[derive(Clone, Default)]
pub struct Codec {
    pub nodes: BTreeMap<u8, Node>,
    pub log: Vec<(u8, u32)>,
}

impl Codec {
    /// Answer one command (codec address ignored: one codec on the link).
    pub fn answer(&mut self, command: u32) -> u32 {
        let nid = ((command >> 20) & 0xFF) as u8;
        let verb12 = ((command >> 8) & 0xFFF) as u16;
        let payload = command & 0xFF;
        let Some(node) = self.nodes.get(&nid) else {
            return 0;
        };
        match verb12 {
            GET_PARAMETER => node.params.get(&(payload as u8)).copied().unwrap_or(0),
            GET_CONFIG_DEFAULT => node.config,
            GET_CONN_LIST => {
                let (per, width) = if node.long_form { (2, 16) } else { (4, 8) };
                let start = payload as usize;
                (0..per).fold(0u32, |word, slot| {
                    let entry = node.conn_raw.get(start + slot).copied().unwrap_or(0);
                    word | u32::from(entry) << (slot * width)
                })
            }
            _ => {
                self.log.push((nid, command & 0xFFFFF));
                0
            }
        }
    }
}

/// The controller's state.
pub struct Card {
    pub regs: Vec<u8>,
    mem: *mut u8,
    mem_len: usize,
    pub codec: Codec,
    corb_rp: u16,
    /// Present codecs (`STATESTS` after reset).
    pub present: u16,
    /// Answer nothing (a dead link).
    pub silent: bool,
    /// Put an unsolicited response before each real one.
    pub unsolicited: bool,
    /// Responses since `RIRBSTS.RINTFL` was last cleared: like QEMU's model,
    /// the card takes no command once `RINTCNT` of them are waiting.
    rirb_count: u16,
    unsolicited_sent: bool,
}

#[derive(Clone)]
pub struct Fake(pub Rc<RefCell<Card>>);

impl Fake {
    pub fn new(memory: &Memory, codec: Codec) -> Fake {
        let mut card = Card {
            regs: std::vec![0u8; 0x400],
            mem: memory.va,
            mem_len: memory.len,
            codec,
            corb_rp: 0,
            present: 1,
            silent: false,
            unsolicited: false,
            rirb_count: 0,
            unsolicited_sent: false,
        };
        let gcap = (OUTPUTS << 12 | INPUTS << 8 | 1) as u16;
        card.put16(GCAP, gcap);
        card.regs[CORBSIZE as usize] = 0x70;
        card.regs[RIRBSIZE as usize] = 0x70;
        Fake(Rc::new(RefCell::new(card)))
    }

    /// Move the output stream's link position on by `bytes`, setting the
    /// completion status at each period boundary crossed.
    pub fn play(&self, bytes: u32) {
        let mut card = self.0.borrow_mut();
        if card.regs[(OUT0 + sd::CTL) as usize] & sdctl::RUN as u8 == 0 {
            return;
        }
        let cbl = card.get32(OUT0 + sd::CBL).max(1);
        let lpib = card.get32(OUT0 + sd::LPIB);
        let next = (lpib + bytes) % cbl;
        card.put32(OUT0 + sd::LPIB, next);
        card.regs[(OUT0 + sd::STS) as usize] |= sdsts::BCIS;
    }
}

impl Card {
    fn get16(&self, at: u32) -> u16 {
        u16::from_le_bytes([self.regs[at as usize], self.regs[at as usize + 1]])
    }

    fn get32(&self, at: u32) -> u32 {
        let a = at as usize;
        u32::from_le_bytes(self.regs[a..a + 4].try_into().unwrap())
    }

    fn put16(&mut self, at: u32, value: u16) {
        self.regs[at as usize..at as usize + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(&mut self, at: u32, value: u32) {
        self.regs[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn host(&self, bus: u64, len: usize) -> *mut u8 {
        let offset = bus.checked_sub(BUS).expect("bus address below the memory") as usize;
        assert!(
            offset + len <= self.mem_len,
            "DMA outside the memory: {bus:#x}+{len}"
        );
        // SAFETY: checked inside the memory.
        unsafe { self.mem.add(offset) }
    }

    fn base(&self, low: u32) -> u64 {
        u64::from(self.get32(low)) | u64::from(self.get32(low + 4)) << 32
    }

    fn respond(&mut self, response: u32, extended: u32) {
        let wp = (self.get16(RIRBWP) + 1) % 256;
        let at = self.host(self.base(RIRBLBASE) + u64::from(wp) * 8, 8);
        // SAFETY: inside the memory (checked by `host`).
        unsafe {
            (at as *mut u32).write_unaligned(response);
            (at.add(4) as *mut u32).write_unaligned(extended);
        }
        self.put16(RIRBWP, wp);
        self.rirb_count += 1;
        if self.rirb_count >= self.rint_count()
            && self.regs[RIRBCTL as usize] & ring::RIRB_RINTCTL != 0
        {
            self.regs[RIRBSTS as usize] |= 1;
        }
    }

    /// `RINTCNT`, where 0 means 256.
    fn rint_count(&self) -> u16 {
        match self.get16(RINTCNT) & 0xFF {
            0 => 256,
            count => count,
        }
    }

    /// Run every command from the read pointer up to the write pointer.
    fn run_corb(&mut self) {
        if self.regs[CORBCTL as usize] & ring::CORB_RUN == 0 {
            return;
        }
        let wp = self.get16(CORBWP) % 256;
        while self.corb_rp != wp {
            // Responses waiting for the flag to be cleared hold the ring.
            if self.rirb_count >= self.rint_count() {
                return;
            }
            if self.unsolicited && !self.unsolicited_sent && !self.silent {
                self.respond(0xDEAD_BEEF, 1 << 4);
                self.unsolicited_sent = true;
                continue;
            }
            self.corb_rp = (self.corb_rp + 1) % 256;
            self.unsolicited_sent = false;
            let at = self.host(self.base(CORBLBASE) + u64::from(self.corb_rp) * 4, 4);
            // SAFETY: inside the memory.
            let command = unsafe { (at as *const u32).read_unaligned() };
            if self.silent {
                continue;
            }
            let response = self.codec.answer(command);
            self.respond(response, 0);
        }
    }
}

impl Regs for Fake {
    fn read8(&self, offset: u32) -> u8 {
        self.0.borrow().regs[offset as usize]
    }

    fn read16(&self, offset: u32) -> u16 {
        let card = self.0.borrow();
        match offset {
            CORBRP => card.get16(CORBRP),
            _ => card.get16(offset),
        }
    }

    fn read32(&self, offset: u32) -> u32 {
        self.0.borrow().get32(offset)
    }

    fn write8(&mut self, offset: u32, value: u8) {
        let mut card = self.0.borrow_mut();
        match offset {
            RIRBSTS => {
                let old = card.regs[RIRBSTS as usize];
                card.regs[RIRBSTS as usize] &= !value;
                if old & 1 != 0 && card.regs[RIRBSTS as usize] & 1 == 0 {
                    card.rirb_count = 0;
                }
            }
            _ if offset == OUT0 + sd::STS => card.regs[offset as usize] &= !value,
            _ if offset == OUT0 + sd::CTL => {
                // Stream reset reads back as set while asserted; leaving it
                // clears the position.
                if value & sdctl::SRST as u8 == 0
                    && card.regs[offset as usize] & sdctl::SRST as u8 != 0
                {
                    card.put32(OUT0 + sd::LPIB, 0);
                }
                card.regs[offset as usize] = value;
            }
            CORBCTL => {
                card.regs[CORBCTL as usize] = value;
                card.run_corb();
            }
            _ => card.regs[offset as usize] = value,
        }
    }

    fn write16(&mut self, offset: u32, value: u16) {
        let mut card = self.0.borrow_mut();
        match offset {
            CORBWP => {
                card.put16(CORBWP, value);
                card.run_corb();
            }
            CORBRP => {
                if value & ring::PTR_RESET != 0 {
                    card.corb_rp = 0;
                }
                card.put16(CORBRP, value & ring::PTR_RESET);
            }
            RIRBWP if value & ring::PTR_RESET != 0 => card.put16(RIRBWP, 0),
            _ => card.put16(offset, value),
        }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        let mut card = self.0.borrow_mut();
        if offset == GCTL {
            card.put32(GCTL, value);
            let present = if value & gctl::CRST != 0 {
                card.present
            } else {
                0
            };
            card.put16(STATESTS, present);
            return;
        }
        card.put32(offset, value);
    }
}

/// Widget capability word: type and flags.
pub fn wcaps(kind: u32, flags: u32) -> u32 {
    kind << 20 | flags
}

/// A node with the given parameters.
pub fn node(params: &[(u8, u32)], conns: &[u16], config: u32) -> Node {
    Node {
        params: params.iter().copied().collect(),
        conn_raw: conns.to_vec(),
        long_form: false,
        config,
    }
}

/// QEMU's `hda-output`: function group 1, a stereo DAC at 2 with its own
/// format and an output amp, a line-out jack at 3 wired to it.
pub fn qemu_output() -> Codec {
    use crate::verbs::caps::*;
    let mut codec = Codec::default();
    codec
        .nodes
        .insert(0, node(&[(param::NODE_COUNT, 1 << 16 | 1)], &[], 0));
    codec.nodes.insert(
        1,
        node(
            &[(param::FUNCTION_TYPE, 1), (param::NODE_COUNT, 2 << 16 | 2)],
            &[],
            0,
        ),
    );
    codec.nodes.insert(
        2,
        node(
            &[
                (
                    param::WIDGET_CAPS,
                    wcaps(OUTPUT, STEREO | OUT_AMP | AMP_OVERRIDE | FORMAT_OVERRIDE),
                ),
                (param::PCM, 1 << 17 | 0x1FC),
                (param::AMP_OUT_CAPS, 0x80 << 24 | 0x4A << 8 | 0x4A),
            ],
            &[],
            0,
        ),
    );
    codec.nodes.insert(
        3,
        node(
            &[
                (param::WIDGET_CAPS, wcaps(PIN, STEREO | CONN_LIST)),
                (param::PIN_CAPS, crate::verbs::pin::CAP_OUTPUT),
                (param::CONN_LIST_LEN, 1),
            ],
            &[2],
            0x0001_4010,
        ),
    );
    codec
}
