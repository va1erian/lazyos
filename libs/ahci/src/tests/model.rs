//! A model AHCI HBA behind the [`Platform`] seam: registers, a flat physical
//! memory, disks, and the commands the driver sends. It panics on what a
//! real HBA would reject (a command issued with `ST` clear, an odd PRDT
//! entry, a PRDT that does not match the sector count), so a driver bug
//! fails the test. [`Behavior`] makes it slow, stuck or dishonest.

use std::cell::{Cell, RefCell};
use std::vec;
use std::vec::Vec;

use crate::cmd::{Header, Prd, HEADER_BYTES, PRDT_OFFSET, PRD_BYTES};
use crate::fis::{ata, H2d, H2D_BYTES};
use crate::regs::{self, bohc, cap2, cmd, ghc, is, px, tfd};
use crate::{Platform, PortPages, MAX_SLOTS};

mod behavior;
mod ident;

pub use behavior::Behavior;
use ident::identify_data;
pub use ident::Ident;

const MEM_BYTES: usize = 24 << 20;
/// Where test buffers start; DMA structures live below.
pub const BUFFER_BASE: u64 = 4 << 20;

/// What sits on a port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Empty,
    Ata,
    Atapi,
    Unknown(u32),
}

pub struct Port {
    pub kind: Kind,
    pub disk: Vec<u8>,
    pub ident: Ident,
    clb: u64,
    fb: u64,
    is: u32,
    cmd: u32,
    tfd: u32,
    sctl: u32,
    serr: u32,
    ci: u32,
    running: bool,
    stopped: bool,
    countdown: u32,
    busy_left: u32,
    settle_left: u32,
    pub comresets: u32,
    pub flushes: u32,
    pub standbys: u32,
    pub commands: Vec<(u8, u64, u32)>,
    pub max_inflight: u32,
}

pub struct State {
    ghc: u32,
    bohc: u32,
    bios_left: u32,
    busy_left: u32,
    mem: Vec<u8>,
    next_page: u64,
    pub ports: Vec<Port>,
    pub behavior: Behavior,
}

pub struct Model {
    pub state: RefCell<State>,
    clock: Cell<u64>,
}

impl Port {
    fn new(kind: Kind, sectors: u64) -> Port {
        Port {
            kind,
            disk: vec![0; (sectors * 512) as usize],
            ident: Ident::default(),
            clb: 0,
            fb: 0,
            is: 0,
            cmd: 0,
            tfd: 0x50,
            sctl: 0,
            serr: 0,
            ci: 0,
            running: false,
            stopped: false,
            countdown: 0,
            busy_left: 0,
            settle_left: 0,
            comresets: 0,
            flushes: 0,
            standbys: 0,
            commands: Vec::new(),
            max_inflight: 0,
        }
    }

    fn sig(&self) -> u32 {
        match self.kind {
            Kind::Ata | Kind::Empty => regs::SIG_ATA,
            Kind::Atapi => regs::SIG_ATAPI,
            Kind::Unknown(sig) => sig,
        }
    }
}

impl Model {
    /// One ATA disk of `sectors` sectors on port 0.
    pub fn new(sectors: u64) -> Model {
        Model::with_ports(vec![Port::new(Kind::Ata, sectors)], Behavior::default())
    }

    pub fn with_ports(ports: Vec<Port>, behavior: Behavior) -> Model {
        Model {
            state: RefCell::new(State {
                ghc: 0,
                bohc: 0,
                bios_left: 0,
                busy_left: 0,
                mem: vec![0; MEM_BYTES],
                next_page: 0x1000,
                ports,
                behavior: Behavior::default(),
            }),
            clock: Cell::new(0),
        }
        .with(behavior)
    }

    pub fn port_of(kind: Kind, sectors: u64) -> Port {
        Port::new(kind, sectors)
    }

    /// Set the behavior, applying the power-on parts to every port.
    pub fn with(self, behavior: Behavior) -> Model {
        {
            let mut state = self.state.borrow_mut();
            state.behavior = behavior;
            for port in &mut state.ports {
                port.busy_left = behavior.busy_polls;
                port.settle_left = behavior.settle_polls;
            }
        }
        self
    }

    pub fn behavior(&self, change: impl FnOnce(&mut Behavior)) {
        change(&mut self.state.borrow_mut().behavior);
    }

    pub fn port<R>(&self, index: usize, f: impl FnOnce(&mut Port) -> R) -> R {
        f(&mut self.state.borrow_mut().ports[index])
    }

    /// A fresh zeroed 4 KiB page of model memory.
    pub fn alloc_page(&self) -> u64 {
        let mut state = self.state.borrow_mut();
        let page = state.next_page;
        state.next_page += 4096;
        assert!(page < BUFFER_BASE, "DMA structures overflow their region");
        page
    }

    /// The last page handed out by [`Model::alloc_page`].
    pub fn last_page(&self) -> u64 {
        self.state.borrow().next_page - 4096
    }

    pub fn pages(&self) -> PortPages {
        PortPages {
            list: self.alloc_page(),
            fis: self.alloc_page() + 0x400,
            tables: core::array::from_fn::<u64, MAX_SLOTS, _>(|_| self.alloc_page()),
            identify: self.alloc_page(),
        }
    }

    pub fn disk_bytes(&self, index: usize) -> Vec<u8> {
        self.state.borrow().ports[index].disk.clone()
    }
}

impl State {
    fn read_mem(&self, phys: u64, buf: &mut [u8]) {
        let at = phys as usize;
        buf.copy_from_slice(&self.mem[at..at + buf.len()]);
    }

    fn write_mem(&mut self, phys: u64, data: &[u8]) {
        let at = phys as usize;
        self.mem[at..at + data.len()].copy_from_slice(data);
    }

    fn port_reg(&mut self, index: usize, offset: usize) -> u32 {
        let behavior = self.behavior;
        if index >= self.ports.len() {
            return 0;
        }
        match offset {
            px::CLB => self.ports[index].clb as u32,
            px::CLBU => (self.ports[index].clb >> 32) as u32,
            px::FB => self.ports[index].fb as u32,
            px::FBU => (self.ports[index].fb >> 32) as u32,
            px::IS => self.ports[index].is,
            px::IE => 0,
            px::CMD => {
                let port = &self.ports[index];
                let mut value = port.cmd & (cmd::ST | cmd::FRE);
                if port.running {
                    value |= cmd::CR;
                }
                if port.cmd & cmd::FRE != 0 {
                    value |= cmd::FR;
                }
                value
            }
            px::TFD => {
                let port = &mut self.ports[index];
                if port.busy_left > 0 {
                    port.busy_left -= 1;
                    return tfd::BSY;
                }
                port.tfd
            }
            px::SIG => self.ports[index].sig(),
            px::SSTS => {
                let port = &mut self.ports[index];
                if port.kind == Kind::Empty || behavior.dead_link && port.comresets > 0 {
                    return 0;
                }
                if port.settle_left > 0 {
                    port.settle_left -= 1;
                    return 1;
                }
                0x113
            }
            px::SCTL => self.ports[index].sctl,
            px::SERR => self.ports[index].serr,
            px::CI => self.read_ci(index),
            _ => 0,
        }
    }

    fn read_ci(&mut self, index: usize) -> u32 {
        let behavior = self.behavior;
        let port = &mut self.ports[index];
        if port.ci != 0 && !behavior.hang && !port.stopped {
            if port.countdown == 0 {
                port.countdown = behavior.latency;
                let slot = port.ci.trailing_zeros() as usize;
                self.complete(index, slot);
            } else {
                port.countdown -= 1;
            }
        }
        self.ports[index].ci
    }

    /// Execute the command in `slot`.
    fn complete(&mut self, index: usize, slot: usize) {
        let behavior = self.behavior;
        let clb = self.ports[index].clb;
        let mut header = [0u8; HEADER_BYTES];
        self.read_mem(clb + (slot * HEADER_BYTES) as u64, &mut header);
        let header = Header::decode(&header);
        let mut fis = [0u8; H2D_BYTES];
        self.read_mem(header.ctba, &mut fis);
        let fis = H2d::decode(&fis).expect("a Register H2D command FIS");
        let mut prds = Vec::new();
        for entry in 0..header.prdtl as usize {
            let mut raw = [0u8; PRD_BYTES];
            self.read_mem(
                header.ctba + (PRDT_OFFSET + entry * PRD_BYTES) as u64,
                &mut raw,
            );
            let prd = Prd::decode(&raw);
            assert!(
                prd.addr.is_multiple_of(2) && prd.bytes.is_multiple_of(2),
                "odd PRDT entry {prd:?}"
            );
            assert!(behavior.s64a || prd.addr + u64::from(prd.bytes) <= 1 << 32);
            prds.push(prd);
        }
        let total: u32 = prds.iter().map(|prd| prd.bytes).sum();
        let count = if fis.count == 0 {
            65536
        } else {
            u32::from(fis.count)
        };
        let mut moved = total;
        match fis.command {
            ata::READ_DMA_EXT | ata::WRITE_DMA_EXT => {
                assert_eq!(total, count * 512, "PRDT does not match the sector count");
                assert_eq!(header.write, fis.command == ata::WRITE_DMA_EXT);
                self.ports[index]
                    .commands
                    .push((fis.command, fis.lba, count));
                let sectors = (self.ports[index].disk.len() / 512) as u64;
                assert!(fis.lba + u64::from(count) <= sectors, "LBA beyond the disk");
                if let Some(bad) = behavior.fail_lba {
                    if (fis.lba..fis.lba + u64::from(count)).contains(&bad) {
                        let port = &mut self.ports[index];
                        port.is |= is::TFES;
                        port.tfd = tfd::ERR | 0x40 << 8 | 0x50;
                        if behavior.busy_after_error {
                            port.tfd |= tfd::BSY;
                        }
                        port.stopped = true;
                        return;
                    }
                }
                let mut disk_at = (fis.lba * 512) as usize;
                for prd in &prds {
                    let len = prd.bytes as usize;
                    if fis.command == ata::READ_DMA_EXT {
                        let data = self.ports[index].disk[disk_at..disk_at + len].to_vec();
                        self.write_mem(prd.addr, &data);
                    } else {
                        let mut data = vec![0u8; len];
                        self.read_mem(prd.addr, &mut data);
                        self.ports[index].disk[disk_at..disk_at + len].copy_from_slice(&data);
                    }
                    disk_at += len;
                }
            }
            ata::FLUSH_CACHE_EXT => self.ports[index].flushes += 1,
            ata::STANDBY_IMMEDIATE => self.ports[index].standbys += 1,
            ata::IDENTIFY_DEVICE => {
                let sectors = (self.ports[index].disk.len() / 512) as u64;
                let data = identify_data(&self.ports[index].ident, sectors);
                self.write_mem(prds[0].addr, &data);
            }
            other => panic!("unexpected ATA command {other:#x}"),
        }
        if behavior.short_prdbc {
            moved -= 2;
        }
        let port = &mut self.ports[index];
        port.ci &= !(1 << slot);
        self.write_mem(clb + (slot * HEADER_BYTES + 4) as u64, &moved.to_le_bytes());
    }

    fn write_port(&mut self, index: usize, offset: usize, value: u32) {
        let behavior = self.behavior;
        if index >= self.ports.len() {
            return;
        }
        let port = &mut self.ports[index];
        match offset {
            px::CLB => port.clb = port.clb & !0xFFFF_FFFF | u64::from(value),
            px::CLBU => port.clb = port.clb & 0xFFFF_FFFF | u64::from(value) << 32,
            px::FB => port.fb = port.fb & !0xFFFF_FFFF | u64::from(value),
            px::FBU => port.fb = port.fb & 0xFFFF_FFFF | u64::from(value) << 32,
            px::IS => port.is &= !value,
            px::SERR => port.serr &= !value,
            px::CMD => {
                let was_running = port.cmd & cmd::ST != 0;
                port.cmd = value & (cmd::ST | cmd::FRE);
                if port.cmd & cmd::ST != 0 && !was_running {
                    port.running = true;
                    port.stopped = false;
                    assert!(port.cmd & cmd::FRE != 0, "ST set without FRE");
                    assert!(port.clb != 0 && port.fb != 0, "ST set before the lists");
                }
                if port.cmd & cmd::ST == 0 && was_running {
                    port.ci = 0;
                    if !behavior.never_stop {
                        port.running = false;
                    }
                }
            }
            px::SCTL => {
                let reset = value & 0xF == regs::sctl::DET_RESET;
                if port.sctl & 0xF == regs::sctl::DET_RESET && !reset {
                    port.comresets += 1;
                    port.tfd = 0x50;
                    port.busy_left = behavior.busy_polls;
                    port.stopped = false;
                }
                port.sctl = value;
            }
            px::CI => {
                assert!(port.running, "command issued with ST clear");
                assert!(!port.stopped || behavior.fail_lba.is_some());
                port.ci |= value;
                port.max_inflight = port.max_inflight.max(port.ci.count_ones());
            }
            _ => {}
        }
    }
}

impl Platform for Model {
    fn read32(&self, offset: usize) -> u32 {
        let mut state = self.state.borrow_mut();
        match offset {
            regs::CAP => {
                let s64a = if state.behavior.s64a {
                    regs::cap::S64A
                } else {
                    0
                };
                (state.ports.len() as u32 - 1) | 31 << regs::cap::NCS_SHIFT | s64a
            }
            regs::GHC => state.ghc,
            regs::IS => 0,
            regs::PI => (1u32 << state.ports.len()) - 1,
            regs::VS => 0x0001_0301,
            regs::CAP2 => u32::from(state.behavior.handoff) * cap2::BOH,
            regs::BOHC => {
                if state.bios_left > 0 {
                    state.bios_left -= 1;
                    if state.bios_left == 0 {
                        state.bohc &= !bohc::BOS;
                        state.busy_left = state.behavior.bios_busy_polls;
                    }
                } else if state.busy_left > 0 {
                    state.busy_left -= 1;
                }
                let busy = if state.busy_left > 0 { bohc::BB } else { 0 };
                state.bohc | busy
            }
            offset if offset >= regs::PORT_BASE => {
                let index = (offset - regs::PORT_BASE) / regs::PORT_STRIDE;
                let reg = (offset - regs::PORT_BASE) % regs::PORT_STRIDE;
                state.port_reg(index, reg)
            }
            _ => 0,
        }
    }

    fn write32(&self, offset: usize, value: u32) {
        let mut state = self.state.borrow_mut();
        match offset {
            regs::GHC => state.ghc = value & (ghc::AE | ghc::IE),
            regs::BOHC => {
                if value & bohc::OOS != 0 && state.bohc & bohc::OOS == 0 {
                    state.bios_left = state.behavior.bios_polls;
                    if state.behavior.bios_polls > 0 {
                        state.bohc |= bohc::BOS;
                    }
                }
                state.bohc = state.bohc & bohc::BOS | value & bohc::OOS;
            }
            offset if offset >= regs::PORT_BASE => {
                let index = (offset - regs::PORT_BASE) / regs::PORT_STRIDE;
                let reg = (offset - regs::PORT_BASE) % regs::PORT_STRIDE;
                state.write_port(index, reg, value);
            }
            _ => {}
        }
    }

    fn read_mem(&self, phys: u64, buf: &mut [u8]) {
        self.state.borrow().read_mem(phys, buf);
    }

    fn write_mem(&self, phys: u64, data: &[u8]) {
        self.state.borrow_mut().write_mem(phys, data);
    }

    fn now_ns(&self) -> u64 {
        self.clock.set(self.clock.get() + 1_000);
        self.clock.get()
    }

    fn relax(&self) {
        self.clock.set(self.clock.get() + 50_000);
    }
}
