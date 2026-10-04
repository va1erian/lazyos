//! A model NVMe controller behind the [`Platform`] seam: registers, a sparse
//! physical memory, a disk, and the admin and NVM commands the driver sends.
//! It checks what a real controller would reject (PRP rules, queue ids, the
//! enable sequence) by panicking, so a driver bug fails the test.

use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::vec;
use std::vec::Vec;

use crate::cmd::{admin, cns, nvm, Command, Completion, CQE_BYTES, SQE_BYTES};
use crate::regs::{self, cc, csts};
use crate::{Pages, Platform, MAX_INFLIGHT, PAGE};

/// How the model misbehaves.
#[derive(Clone, Copy, Default)]
pub struct Behavior {
    /// `CSTS.RDY` never follows `CC.EN`.
    pub never_ready: bool,
    /// Enabling raises `CSTS.CFS`.
    pub fatal_on_enable: bool,
    /// I/O commands are fetched but never completed.
    pub hang_io: bool,
    /// I/O commands wait for [`Model::run_pending`] (several in flight).
    pub defer_io: bool,
    /// A read or write touching this LBA fails with a media error.
    pub fail_lba: Option<u64>,
    /// Post a completion with a bogus command id before each real one.
    pub stray_completions: bool,
    /// Identify Namespace reports this LBA data size shift (default 9).
    pub lba_shift: Option<u8>,
    /// Identify Controller's MDTS (default 0: unlimited).
    pub mdts: u8,
    /// Volatile write cache present.
    pub vwc: bool,
    /// `CAP.MQES` (0-based; default 1023).
    pub mqes: Option<u16>,
    /// The shutdown notification never completes.
    pub shutdown_hangs: bool,
}

#[derive(Clone, Copy)]
struct Sq {
    base: u64,
    depth: u16,
    head: u16,
    cqid: u16,
}

#[derive(Clone, Copy)]
struct Cq {
    base: u64,
    depth: u16,
    tail: u16,
    /// The head the host last reported through its doorbell.
    head: u16,
    phase: bool,
}

pub struct State {
    cc: u32,
    csts: u32,
    aqa: u32,
    asq: u64,
    acq: u64,
    mem: HashMap<u64, Box<[u8; PAGE as usize]>>,
    pub disk: Vec<u8>,
    sqs: [Option<Sq>; 2],
    cqs: [Option<Cq>; 2],
    pending: Vec<Command>,
    pub behavior: Behavior,
    pub flushes: u32,
    pub io_commands: u32,
    pub max_pending: usize,
    pub shutdowns: u32,
}

pub struct Model {
    pub state: RefCell<State>,
    clock: Cell<u64>,
    next_page: Cell<u64>,
}

impl Model {
    pub fn new(disk_blocks: usize, behavior: Behavior) -> Model {
        let shift = behavior.lba_shift.unwrap_or(9);
        Model {
            state: RefCell::new(State {
                cc: 0,
                csts: 0,
                aqa: 0,
                asq: 0,
                acq: 0,
                mem: HashMap::new(),
                disk: vec![0; disk_blocks << shift],
                sqs: [None; 2],
                cqs: [None; 2],
                pending: Vec::new(),
                behavior,
                flushes: 0,
                io_commands: 0,
                max_pending: 0,
                shutdowns: 0,
            }),
            clock: Cell::new(0),
            next_page: Cell::new(0x1_0000_0000),
        }
    }

    /// A fresh page of "physical" memory.
    pub fn alloc_page(&self) -> u64 {
        let page = self.next_page.get();
        // Leave holes so pages are never accidentally contiguous.
        self.next_page.set(page + 3 * PAGE);
        self.state.borrow_mut().page(page);
        page
    }

    pub fn pages(&self) -> Pages {
        let mut prp_lists = [0; MAX_INFLIGHT];
        for list in prp_lists.iter_mut() {
            *list = self.alloc_page();
        }
        Pages {
            admin_sq: self.alloc_page(),
            admin_cq: self.alloc_page(),
            io_sq: self.alloc_page(),
            io_cq: self.alloc_page(),
            identify: self.alloc_page(),
            prp_lists,
        }
    }

    /// Complete every deferred I/O command.
    pub fn run_pending(&self) {
        let mut state = self.state.borrow_mut();
        let pending = core::mem::take(&mut state.pending);
        for command in pending {
            state.execute_io(command);
        }
    }
}

impl State {
    fn page(&mut self, phys: u64) -> &mut [u8; PAGE as usize] {
        self.mem
            .entry(phys & !(PAGE - 1))
            .or_insert_with(|| Box::new([0; PAGE as usize]))
    }

    pub fn read(&mut self, phys: u64, out: &mut [u8]) {
        for (index, byte) in out.iter_mut().enumerate() {
            let at = phys + index as u64;
            *byte = self.page(at)[(at % PAGE) as usize];
        }
    }

    pub fn write(&mut self, phys: u64, data: &[u8]) {
        for (index, &byte) in data.iter().enumerate() {
            let at = phys + index as u64;
            self.page(at)[(at % PAGE) as usize] = byte;
        }
    }

    fn cap(&self) -> u64 {
        let mqes = u64::from(self.behavior.mqes.unwrap_or(1023));
        // MQES, CQR, TO = 2 (1 s), DSTRD = 0, CSS = NVM, MPSMIN = 0, MPSMAX = 4.
        mqes | 1 << 16 | 2 << 24 | 1 << 37 | 4 << 52
    }

    fn set_cc(&mut self, value: u32) {
        let was = self.cc;
        self.cc = value;
        if was & cc::EN == 0 && value & cc::EN != 0 {
            assert_eq!(
                value & !cc::SHN_MASK,
                regs::cc_enable(),
                "unexpected CC on enable"
            );
            if self.behavior.fatal_on_enable {
                self.csts |= csts::CFS;
                return;
            }
            if self.behavior.never_ready {
                return;
            }
            let sq_depth = (self.aqa & 0xFFF) as u16 + 1;
            let cq_depth = ((self.aqa >> 16) & 0xFFF) as u16 + 1;
            assert!(
                self.asq.is_multiple_of(PAGE) && self.acq.is_multiple_of(PAGE),
                "admin queues unaligned"
            );
            self.sqs = [
                Some(Sq {
                    base: self.asq,
                    depth: sq_depth,
                    head: 0,
                    cqid: 0,
                }),
                None,
            ];
            self.cqs = [
                Some(Cq {
                    base: self.acq,
                    depth: cq_depth,
                    tail: 0,
                    head: 0,
                    phase: true,
                }),
                None,
            ];
            self.csts |= csts::RDY;
        } else if was & cc::EN != 0 && value & cc::EN == 0 {
            self.sqs = [None; 2];
            self.cqs = [None; 2];
            self.pending.clear();
            self.csts &= !(csts::RDY | csts::SHST_MASK);
        }
        if value & cc::SHN_MASK != 0 && was & cc::SHN_MASK == 0 {
            self.shutdowns += 1;
            if !self.behavior.shutdown_hangs {
                self.csts = (self.csts & !csts::SHST_MASK) | csts::SHST_COMPLETE;
            }
        }
    }

    fn doorbell(&mut self, offset: usize, value: u32) {
        let index = (offset - regs::DOORBELLS) / 4;
        let qid = index / 2;
        if index % 2 == 1 {
            // Completion head: just check it names a slot of the queue.
            let mut cq = self.cqs[qid].expect("head doorbell of a missing CQ");
            assert!(value < u32::from(cq.depth), "CQ head past the queue");
            cq.head = value as u16;
            self.cqs[qid] = Some(cq);
            return;
        }
        let mut sq = self.sqs[qid].expect("tail doorbell of a missing SQ");
        assert!(value < u32::from(sq.depth), "SQ tail past the queue");
        while u32::from(sq.head) != value {
            let mut raw = [0u8; SQE_BYTES];
            self.read(sq.base + u64::from(sq.head) * SQE_BYTES as u64, &mut raw);
            sq.head = (sq.head + 1) % sq.depth;
            self.sqs[qid] = Some(sq);
            let command = Command::decode(&raw);
            if qid == 0 {
                let (status, result) = self.execute_admin(&command);
                self.complete(0, command.cid, status, result);
            } else if self.behavior.hang_io {
                // Fetched, never completed.
            } else if self.behavior.defer_io {
                self.pending.push(command);
                self.max_pending = self.max_pending.max(self.pending.len());
            } else {
                self.execute_io(command);
            }
            sq = self.sqs[qid].expect("queue deleted mid-fetch");
        }
    }

    fn complete(&mut self, sqid: u16, cid: u16, status: (u8, u8), result: u32) {
        let sq = self.sqs[usize::from(sqid)].expect("completion for a missing SQ");
        let cqid = usize::from(sq.cqid);
        let mut cq = self.cqs[cqid].expect("SQ bound to a missing CQ");
        // A real controller never posts into a full queue; the host must
        // keep fewer commands outstanding than the queue holds.
        assert!(
            (cq.tail + 1) % cq.depth != cq.head,
            "completion queue {cqid} overflowed"
        );
        let entry = Completion {
            result,
            sq_head: sq.head,
            sq_id: sqid,
            cid,
            phase: cq.phase,
            sct: status.0,
            sc: status.1,
            dnr: status != (0, 0),
        };
        self.write(
            cq.base + u64::from(cq.tail) * CQE_BYTES as u64,
            &entry.encode(),
        );
        cq.tail += 1;
        if cq.tail == cq.depth {
            cq.tail = 0;
            cq.phase = !cq.phase;
        }
        self.cqs[cqid] = Some(cq);
    }

    fn execute_admin(&mut self, command: &Command) -> ((u8, u8), u32) {
        match command.opcode {
            admin::IDENTIFY => {
                let page = match command.cdw[0] {
                    cns::CONTROLLER => identify_controller(&self.behavior),
                    cns::NAMESPACE if command.nsid == 1 => {
                        identify_namespace(&self.behavior, self.disk.len())
                    }
                    cns::NAMESPACE => vec![0; 4096],
                    _ => return ((0, 0x02), 0), // invalid field
                };
                assert!(
                    command.prp1.is_multiple_of(PAGE),
                    "identify buffer unaligned"
                );
                self.write(command.prp1, &page);
                ((0, 0), 0)
            }
            admin::SET_FEATURES => ((0, 0), 0), // one queue of each granted
            admin::CREATE_CQ => {
                let qid = command.cdw[0] & 0xFFFF;
                let depth = (command.cdw[0] >> 16) as u16 + 1;
                assert_eq!(qid, 1, "only queue 1 expected");
                assert_eq!(command.cdw[1] & 1, 1, "CQ not physically contiguous");
                assert_eq!(command.cdw[1] & 2, 0, "CQ asked for interrupts");
                self.cqs[1] = Some(Cq {
                    base: command.prp1,
                    depth,
                    tail: 0,
                    head: 0,
                    phase: true,
                });
                ((0, 0), 0)
            }
            admin::CREATE_SQ => {
                let qid = command.cdw[0] & 0xFFFF;
                let depth = (command.cdw[0] >> 16) as u16 + 1;
                let cqid = (command.cdw[1] >> 16) as u16;
                assert_eq!(qid, 1, "only queue 1 expected");
                if self.cqs[usize::from(cqid)].is_none() {
                    return ((1, 0x00), 0); // completion queue invalid
                }
                self.sqs[1] = Some(Sq {
                    base: command.prp1,
                    depth,
                    head: 0,
                    cqid,
                });
                ((0, 0), 0)
            }
            _ => ((0, 0x01), 0), // invalid opcode
        }
    }

    fn execute_io(&mut self, command: Command) {
        self.io_commands += 1;
        if self.behavior.stray_completions {
            self.complete(1, command.cid ^ 0x8000, (0, 0x06), 0);
        }
        let status = match command.opcode {
            nvm::FLUSH => {
                self.flushes += 1;
                (0, 0)
            }
            nvm::READ | nvm::WRITE => self.read_write(&command),
            _ => (0, 0x01),
        };
        self.complete(1, command.cid, status, 0);
    }

    fn read_write(&mut self, command: &Command) -> (u8, u8) {
        assert_eq!(command.nsid, 1);
        let block = 1usize << self.behavior.lba_shift.unwrap_or(9);
        let lba = u64::from(command.cdw[0]) | u64::from(command.cdw[1]) << 32;
        let blocks = (command.cdw[2] & 0xFFFF) as usize + 1;
        let bytes = blocks * block;
        let start = lba as usize * block;
        if start + bytes > self.disk.len() {
            return (0, 0x80); // LBA out of range
        }
        if let Some(bad) = self.behavior.fail_lba {
            if (lba..lba + blocks as u64).contains(&bad) {
                return (2, 0x81); // unrecovered read error
            }
        }
        let pieces = self.walk_prps(command.prp1, command.prp2, bytes);
        let mut at = start;
        for (phys, len) in pieces {
            if command.opcode == nvm::WRITE {
                let mut data = vec![0u8; len];
                self.read(phys, &mut data);
                self.disk[at..at + len].copy_from_slice(&data);
            } else {
                let data = self.disk[at..at + len].to_vec();
                self.write(phys, &data);
            }
            at += len;
        }
        (0, 0)
    }

    /// The data pieces of a PRP pair, checked against NVMe 1.4 section 4.3.
    fn walk_prps(&mut self, prp1: u64, prp2: u64, bytes: usize) -> Vec<(u64, usize)> {
        assert_eq!(prp1 % 4, 0, "PRP1 not dword aligned");
        let first = ((PAGE - prp1 % PAGE) as usize).min(bytes);
        let mut pieces = vec![(prp1, first)];
        let mut left = bytes - first;
        if left == 0 {
            return pieces;
        }
        if left <= PAGE as usize {
            assert_eq!(prp2 % PAGE, 0, "PRP2 entry not page aligned");
            pieces.push((prp2, left));
            return pieces;
        }
        assert_eq!(prp2 % 8, 0, "PRP list pointer not qword aligned");
        let mut at = prp2;
        while left > 0 {
            assert!(
                !at.is_multiple_of(PAGE) || at == prp2,
                "PRP list ran into the next page (no chaining)"
            );
            let mut raw = [0u8; 8];
            self.read(at, &mut raw);
            let entry = u64::from_le_bytes(raw);
            assert_eq!(entry % PAGE, 0, "PRP list entry not page aligned");
            let len = left.min(PAGE as usize);
            pieces.push((entry, len));
            left -= len;
            at += 8;
        }
        pieces
    }
}

impl Platform for Model {
    fn read32(&self, offset: usize) -> u32 {
        let state = self.state.borrow();
        match offset {
            regs::CAP => state.cap() as u32,
            o if o == regs::CAP + 4 => (state.cap() >> 32) as u32,
            regs::VS => 0x0001_0400,
            regs::CC => state.cc,
            regs::CSTS => state.csts,
            regs::AQA => state.aqa,
            _ => 0,
        }
    }

    fn write32(&self, offset: usize, value: u32) {
        let mut state = self.state.borrow_mut();
        match offset {
            regs::CC => state.set_cc(value),
            regs::AQA => state.aqa = value,
            regs::ASQ => state.asq = (state.asq & !0xFFFF_FFFF) | u64::from(value),
            o if o == regs::ASQ + 4 => {
                state.asq = (state.asq & 0xFFFF_FFFF) | u64::from(value) << 32
            }
            regs::ACQ => state.acq = (state.acq & !0xFFFF_FFFF) | u64::from(value),
            o if o == regs::ACQ + 4 => {
                state.acq = (state.acq & 0xFFFF_FFFF) | u64::from(value) << 32
            }
            regs::INTMS | regs::INTMC => {}
            o if o >= regs::DOORBELLS => state.doorbell(o, value),
            _ => panic!("write to read-only or unknown register {offset:#x}"),
        }
    }

    fn read_mem(&self, phys: u64, buf: &mut [u8]) {
        self.state.borrow_mut().read(phys, buf);
    }

    fn write_mem(&self, phys: u64, data: &[u8]) {
        self.state.borrow_mut().write(phys, data);
    }

    fn now_ns(&self) -> u64 {
        // Every look at the clock costs a millisecond: timeouts end quickly.
        let now = self.clock.get() + 1_000_000;
        self.clock.set(now);
        now
    }
}

fn put(page: &mut [u8], at: usize, bytes: &[u8]) {
    page[at..at + bytes.len()].copy_from_slice(bytes);
}

/// An Identify Controller page for `behavior`.
pub fn identify_controller(behavior: &Behavior) -> Vec<u8> {
    let mut page = vec![0u8; 4096];
    put(&mut page, 0, &0x1B36u16.to_le_bytes());
    put(&mut page, 4, b"lazyos-model        ");
    put(&mut page, 24, b"LazyOS model NVMe controller            ");
    put(&mut page, 64, b"1.0     ");
    page[77] = behavior.mdts;
    page[512] = 0x66;
    page[513] = 0x44;
    put(&mut page, 516, &1u32.to_le_bytes());
    page[525] = u8::from(behavior.vwc);
    page
}

/// An Identify Namespace page for a disk of `bytes` bytes.
pub fn identify_namespace(behavior: &Behavior, bytes: usize) -> Vec<u8> {
    let shift = behavior.lba_shift.unwrap_or(9);
    let blocks = (bytes >> shift) as u64;
    let mut page = vec![0u8; 4096];
    put(&mut page, 0, &blocks.to_le_bytes());
    put(&mut page, 8, &blocks.to_le_bytes());
    put(&mut page, 16, &blocks.to_le_bytes());
    page[25] = 0; // one format
    page[26] = 0; // format 0 in use
    put(&mut page, 128, &(u32::from(shift) << 16).to_le_bytes());
    page
}
