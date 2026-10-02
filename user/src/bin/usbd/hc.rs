//! The host controller: find and claim the xHCI function, reset and start
//! it, run commands, and pump the event ring (xHCI 4.2, 4.6, 4.9).
//!
//! Polling only (the plan's first step): the event ring is read on every loop
//! pass and after every doorbell. Events nobody is waiting for yet (a
//! transfer completing while a command runs, a port change) are kept in a
//! small queue and handed out later.

use alloc::collections::VecDeque;

use user::dev::{self, Row};
use user::sys;
use xhci::regs::{self, cap, op, rt, Mmio, Structural};
use xhci::ring::{erst_entry, EventRing, ProducerRing, RawMem};
use xhci::trb::{self, code, kind, Trb};

use super::mem::{Bar, Region, PAGE};
use super::Error;

/// PCI class of an xHCI controller: serial bus, USB, xHCI programming
/// interface.
const CLASS: (u8, u8, u8) = (0x0C, 0x03, 0x30);
/// PCI command register and the bits the driver sets.
const PCI_COMMAND: u64 = 4;
const PCI_MEMORY: u64 = 1 << 1;
const PCI_BUS_MASTER: u64 = 1 << 2;

/// Slots this driver enables (one per device it can drive at once).
pub(super) const MAX_SLOTS: u8 = 8;
/// TRBs per command and event ring.
const RING_TRBS: usize = 64;
/// Events kept for a later taker; older ones are dropped (and counted).
const PENDING_CAP: usize = 64;
/// How long a reset, a halt or a command may take (PIT ticks, 100 Hz).
const TIMEOUT_TICKS: u64 = 500;

/// Layout of the controller's own region: DCBAA (256 slots * 8), the
/// one-entry ERST, the command ring, the event ring.
const DCBAA: usize = 0;
const ERST: usize = 2048;
const COMMAND_RING: usize = 4096;
const EVENT_RING: usize = COMMAND_RING + RING_TRBS * 16;
const CORE_BYTES: usize = EVENT_RING + RING_TRBS * 16;

/// What the controller reported about itself.
#[derive(Clone, Copy, Debug)]
pub(super) struct Info {
    pub(super) version: u16,
    pub(super) ports: u8,
    pub(super) slots: u8,
    pub(super) scratchpads: u16,
    pub(super) context_64: bool,
}

pub(super) struct Hc {
    pub(super) handle: u64,
    bar: Bar,
    op: usize,
    rt: usize,
    db: u32,
    pub(super) info: Info,
    core: Region,
    /// The scratchpad array and buffers, kept for as long as the controller
    /// may use them (until exit).
    scratchpad: Option<Region>,
    commands: ProducerRing<RawMem>,
    events: EventRing<RawMem>,
    pending: VecDeque<Trb>,
    /// Events dropped because nobody took them in time.
    pub(super) dropped: u64,
    /// Device regions by slot id, kept across detach and reused on the next
    /// attach that gets the same slot (freeing one would stop the device).
    pool: [Option<Region>; MAX_SLOTS as usize + 1],
    /// Device regions ever allocated: bounded by the slot count, whatever
    /// the hot-plug churn (the harness checks it).
    pub(super) regions: u32,
}

/// Sleep one PIT tick (userspace has no sleep syscall; `wait` doubles as one).
pub(super) fn nap() {
    let _ = sys::wait(sys::clock() + 1);
}

/// The first xHCI function in the device list.
fn find() -> Result<Row, Error> {
    let mut rows = [[0u64; dev::ROW_WORDS]; dev::MAX_ROWS];
    let total = dev::list(&mut rows).map_err(Error::Dev)?;
    rows.iter()
        .take(total.min(rows.len()))
        .map(Row::from_words)
        .find(|row| {
            row.flags & dev::row_flag::PCI != 0 && (row.class, row.subclass, row.prog_if) == CLASS
        })
        .ok_or(Error::NoController)
}

impl Hc {
    /// Claim the controller, map BAR 0, reset it, set up the DCBAA, the
    /// scratchpads, the command and event rings, and start it.
    pub(super) fn open() -> Result<Hc, Error> {
        let row = find()?;
        // BAR 0 must be a present memory BAR.
        if row.bar_meta[0] & 0b11 != 0b01 || row.bar_len[0] < 0x1000 {
            return Err(Error::Bar);
        }
        let handle = dev::claim(row.id, None, false).map_err(Error::Dev)?;
        let command = dev::cfg_read(handle, PCI_COMMAND, 2).map_err(Error::Dev)?;
        dev::cfg_write(
            handle,
            PCI_COMMAND,
            2,
            u64::from(command) | PCI_MEMORY | PCI_BUS_MASTER,
        )
        .map_err(Error::Dev)?;
        let base = dev::map_bar(handle, 0).map_err(Error::Dev)?;
        // SAFETY: the kernel mapped all of BAR 0 for the claim, which lives
        // until this task exits.
        let bar = unsafe { Bar::new(base, row.bar_len[0] as usize) };
        let core = Region::alloc(handle, CORE_BYTES)?;
        let commands =
            ProducerRing::new(core.ring(COMMAND_RING, RING_TRBS)).map_err(Error::Xhci)?;
        let events = EventRing::new(core.ring(EVENT_RING, RING_TRBS)).map_err(Error::Xhci)?;
        let caplength = (bar.read32(cap::CAPLENGTH) & 0xFF) as usize;
        let mut hc = Hc {
            handle,
            op: caplength,
            rt: (bar.read32(cap::RTSOFF) & !0x1F) as usize,
            db: bar.read32(cap::DBOFF),
            info: Info {
                version: (bar.read32(cap::CAPLENGTH) >> 16) as u16,
                ports: 0,
                slots: 0,
                scratchpads: 0,
                context_64: false,
            },
            bar,
            core,
            scratchpad: None,
            commands,
            events,
            pending: VecDeque::new(),
            dropped: 0,
            pool: [const { None }; MAX_SLOTS as usize + 1],
            regions: 0,
        };
        hc.reset()?;
        hc.configure()?;
        hc.start()?;
        Ok(hc)
    }

    fn opreg(&self, offset: usize) -> u32 {
        self.bar.read32(self.op + offset)
    }

    fn set_opreg(&mut self, offset: usize, value: u32) {
        self.bar.write32(self.op + offset, value);
    }

    /// Wait until `done` holds, napping between reads.
    fn until(&self, what: &'static str, done: impl Fn(&Hc) -> bool) -> Result<(), Error> {
        let deadline = sys::clock() + TIMEOUT_TICKS;
        while !done(self) {
            if sys::clock() > deadline {
                return Err(Error::Timeout(what));
            }
            nap();
        }
        Ok(())
    }

    /// Halt (if running) and reset the controller.
    fn reset(&mut self) -> Result<(), Error> {
        self.until("controller ready", |hc| {
            hc.opreg(op::USBSTS) & op::STS_CNR == 0
        })?;
        if self.opreg(op::USBSTS) & op::STS_HALTED == 0 {
            let cmd = self.opreg(op::USBCMD);
            self.set_opreg(op::USBCMD, cmd & !op::CMD_RUN);
            self.until("halt", |hc| hc.opreg(op::USBSTS) & op::STS_HALTED != 0)?;
        }
        self.set_opreg(op::USBCMD, op::CMD_RESET);
        self.until("reset", |hc| {
            hc.opreg(op::USBCMD) & op::CMD_RESET == 0 && hc.opreg(op::USBSTS) & op::STS_CNR == 0
        })
    }

    /// Program slots, the DCBAA, scratchpads and both rings (xHCI 4.2).
    fn configure(&mut self) -> Result<(), Error> {
        let structural = Structural::decode(self.bar.read32(cap::HCSPARAMS1));
        self.info.ports = structural.max_ports;
        self.info.slots = structural.max_slots.min(MAX_SLOTS);
        self.info.scratchpads = regs::scratchpad_count(self.bar.read32(cap::HCSPARAMS2));
        self.info.context_64 = regs::context_64(self.bar.read32(cap::HCCPARAMS1));
        // Only 4 KiB pages are supported (every DMA buffer is 4 KiB-aligned).
        if self.opreg(op::PAGESIZE) & 1 == 0 {
            return Err(Error::PageSize);
        }
        if self.info.slots == 0 || self.info.ports == 0 {
            return Err(Error::NoPorts);
        }
        self.set_opreg(op::CONFIG, u32::from(self.info.slots));
        self.scratchpads()?;
        let dcbaa = self.core.bus(DCBAA);
        self.bar.write64(self.op + op::DCBAAP, dcbaa);
        self.bar
            .write64(self.op + op::CRCR, self.commands.dequeue_pointer());
        // Interrupter 0's event ring: ERSTSZ, ERDP, then ERSTBA last (4.9.4).
        let (segment, size) = self.events.segment();
        let entry = erst_entry(segment, size);
        self.core.dwords(ERST, 4).copy_from_slice(&entry);
        let interrupter = self.rt + rt::INTERRUPTERS;
        self.bar.write32(interrupter + rt::ERSTSZ, 1);
        self.bar.write64(interrupter + rt::ERDP, segment);
        let erst = self.core.bus(ERST);
        self.bar.write64(interrupter + rt::ERSTBA, erst);
        Ok(())
    }

    /// Hand the controller the scratchpad buffers it asked for (4.20).
    fn scratchpads(&mut self) -> Result<(), Error> {
        let count = usize::from(self.info.scratchpads);
        if count == 0 {
            return Ok(());
        }
        // One page for the array, then one page per buffer.
        let mut pages = Region::alloc(self.handle, (count + 1) * PAGE)?;
        for index in 0..count {
            let buffer = pages.bus((index + 1) * PAGE);
            pages.write_u64(index * 8, buffer);
        }
        let array = pages.bus(0);
        self.core.write_u64(DCBAA, array);
        self.scratchpad = Some(pages);
        Ok(())
    }

    fn start(&mut self) -> Result<(), Error> {
        let cmd = self.opreg(op::USBCMD);
        self.set_opreg(op::USBCMD, cmd | op::CMD_RUN);
        self.until("run", |hc| hc.opreg(op::USBSTS) & op::STS_HALTED == 0)?;
        // A No Op proves the command and event rings work end to end.
        self.command(trb::no_op_command()).map(|_| ())
    }

    /// A zeroed region of `len` bytes for the device in `slot`: the slot's
    /// pooled one when it has one (every device region is the same size),
    /// else a new allocation.
    pub(super) fn take_region(&mut self, slot: u8, len: usize) -> Result<Region, Error> {
        let entry = self
            .pool
            .get_mut(usize::from(slot))
            .ok_or(Error::Completion(kind::ENABLE_SLOT, 0))?;
        if let Some(mut region) = entry.take() {
            region.zero();
            return Ok(region);
        }
        self.regions += 1;
        Region::alloc(self.handle, len)
    }

    /// Return a detached device's region for its slot's next device. Only
    /// after Disable Slot: the controller no longer reads or writes it.
    pub(super) fn give_region(&mut self, slot: u8, region: Region) {
        if let Some(entry) = self.pool.get_mut(usize::from(slot)) {
            *entry = Some(region);
        }
    }

    /// Point DCBAA entry `slot` at a device context.
    pub(super) fn set_device_context(&mut self, slot: u8, context: u64) {
        self.core.write_u64(DCBAA + usize::from(slot) * 8, context);
    }

    /// Ring doorbell `slot` (0 for the command ring) for endpoint `target`.
    pub(super) fn doorbell(&mut self, slot: u8, target: u8) {
        let at = regs::doorbell(self.db, slot);
        self.bar.write32(at, u32::from(target));
    }

    /// Run one command and return its completion event, which must report
    /// success.
    pub(super) fn command(&mut self, command: Trb) -> Result<Trb, Error> {
        let pointer = self
            .commands
            .enqueue(&[command], false)
            .map_err(Error::Xhci)?;
        self.doorbell(0, 0);
        let event =
            self.wait(|e| e.kind() == kind::COMMAND_COMPLETION && e.parameter == pointer)?;
        self.commands.retire(pointer).map_err(Error::Xhci)?;
        match event.completion_code() {
            code::SUCCESS => Ok(event),
            other => Err(Error::Completion(command.kind(), other)),
        }
    }

    /// Wait for the first event matching `wanted`, keeping the others.
    pub(super) fn wait(&mut self, wanted: impl Fn(&Trb) -> bool) -> Result<Trb, Error> {
        let deadline = sys::clock() + TIMEOUT_TICKS;
        loop {
            self.pump();
            if let Some(at) = self.pending.iter().position(&wanted) {
                return Ok(self.pending.remove(at).unwrap_or_default());
            }
            if sys::clock() > deadline {
                return Err(Error::Timeout("event"));
            }
            nap();
        }
    }

    /// Move every new event into the pending queue and tell the controller
    /// how far the driver got.
    pub(super) fn pump(&mut self) {
        let mut moved = false;
        while let Some(event) = self.events.pop() {
            if self.pending.len() == PENDING_CAP {
                self.pending.pop_front();
                self.dropped += 1;
            }
            self.pending.push_back(event);
            moved = true;
        }
        if moved {
            let erdp = self.events.erdp();
            let at = self.rt + rt::INTERRUPTERS + rt::ERDP;
            self.bar.write64(at, erdp);
        }
    }

    /// Drop every queued transfer event of `slot`. Called after Disable Slot:
    /// the slot's next device reuses its memory at the same bus addresses,
    /// so a stale completion could otherwise look like one of its own.
    pub(super) fn discard_slot(&mut self, slot: u8) {
        self.pump();
        self.pending
            .retain(|event| !(event.kind() == kind::TRANSFER_EVENT && event.slot() == slot));
    }

    /// Take the oldest pending event, if any.
    pub(super) fn next_event(&mut self) -> Option<Trb> {
        self.pump();
        self.pending.pop_front()
    }

    /// `PORTSC` of `port` (1-based).
    pub(super) fn portsc(&self, port: u8) -> u32 {
        self.opreg(op::PORTS + usize::from(port - 1) * op::PORT_STRIDE)
    }

    pub(super) fn set_portsc(&mut self, port: u8, value: u32) {
        self.set_opreg(op::PORTS + usize::from(port - 1) * op::PORT_STRIDE, value);
    }
}
