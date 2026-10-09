//! One host controller: find and claim the xHCI function, take it from the
//! BIOS, reset and start it, run commands, and pump the event ring (xHCI
//! 4.2, 4.6, 4.9, 4.22.1).
//!
//! The event ring is read on every loop pass and after every doorbell, and
//! interrupter 0 raises the claim's interrupt line when an event lands, so an
//! idle driver sleeps until then (`irq.rs`, P3.7). Events nobody is waiting
//! for yet (a transfer completing while a command runs, a port change) are
//! kept in a queue and handed out later.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use user::dev::{self, Row};
use user::messenger::Endpoint;
use user::sys;
use xhci::extcap::{self, Handoff, Ports};
use xhci::regs::{self, cap, op, rt, Mmio, Structural};
use xhci::ring::{erst_entry, EventRing, ProducerRing, RawMem};
use xhci::trb::{self, code, kind, Trb};

use super::mem::{Bar, Region, PAGE};
use super::Error;

#[path = "hc/dump.rs"]
mod dump;

/// PCI class of an xHCI controller: serial bus, USB, xHCI programming
/// interface.
const CLASS: (u8, u8, u8) = (0x0C, 0x03, 0x30);
/// PCI command register and the bits the driver sets.
const PCI_COMMAND: u64 = 4;
const PCI_MEMORY: u64 = 1 << 1;
const PCI_BUS_MASTER: u64 = 1 << 2;

/// Slots this driver enables per controller (one per device or hub it can
/// drive at once). The kernel records at most 16 DMA buffers per claim
/// (`kernel/src/dev/claims.rs`): the core region, the scratchpads and one
/// region per slot must fit.
pub(super) const MAX_SLOTS: u8 = 12;
/// TRBs in the command ring and in the event ring.
const COMMAND_TRBS: usize = 64;
const EVENT_TRBS: usize = 256;
/// Events kept for a later taker; older ones are dropped (and counted). Each
/// pipe has a bounded number of transfers in flight, so this only overflows
/// on a storm of port changes.
const PENDING_CAP: usize = 512;
/// How long a halt, a command or an event may take (PIT ticks, 100 Hz).
const TIMEOUT_TICKS: u64 = 500;
/// How long a controller reset may take: some Intel controllers need
/// seconds (Linux allows 10 s for them).
const RESET_TICKS: u64 = 1000;
/// How long the BIOS gets to let go of the controller (Linux: 1 s).
const HANDOFF_TICKS: u64 = 100;

/// Layout of the controller's own region: DCBAA (256 slots * 8), the
/// one-entry ERST, the command ring, the event ring.
const DCBAA: usize = 0;
const ERST: usize = 2048;
const COMMAND_RING: usize = 4096;
const EVENT_RING: usize = COMMAND_RING + COMMAND_TRBS * 16;
const CORE_BYTES: usize = EVENT_RING + EVENT_TRBS * 16;

/// What the controller reported about itself.
#[derive(Clone, Copy, Debug)]
pub(super) struct Info {
    pub(super) version: u16,
    pub(super) ports: u8,
    pub(super) slots: u8,
    pub(super) scratchpads: u16,
    pub(super) context_64: bool,
    pub(super) addressing_64: bool,
    pub(super) port_power: bool,
    pub(super) handoff: Handoff,
}

pub(super) struct Hc {
    /// This controller's number in the markers (`hc=`, `port=<hc>-...`).
    pub(super) index: usize,
    pub(super) handle: u64,
    bar: Bar,
    op: usize,
    rt: usize,
    db: u32,
    pub(super) info: Info,
    /// Which root ports are USB 2 and which USB 3, and their speed IDs.
    pub(super) ports: Ports,
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
    /// The bulk data buffer this controller's sticks share (`msc.rs`), made
    /// on first use and kept: they are served one transfer at a time.
    bulk: Option<Region>,
    /// Where the kernel posts this claim's interrupts, once armed (`irq.rs`);
    /// `None` polls.
    pub(super) irq: Option<Endpoint>,
}

/// The bulk window: the most one Normal TRB moves, and an alignment it never
/// crosses (xHCI 4.11.7.1). Its region is twice as large so an aligned
/// window always fits.
pub(super) const BULK_WINDOW: usize = 64 * 1024;

/// Sleep one PIT tick (userspace has no sleep syscall; `wait` doubles as one).
pub(super) fn nap() {
    let _ = sys::wait(sys::clock() + 1);
}

/// Sleep at least `ms` milliseconds (the tick is 10 ms; one more tick
/// covers a partly elapsed current one). USB timings are minimums.
pub(super) fn sleep_ms(ms: u64) {
    let until = sys::clock() + ms.div_ceil(10) + 1;
    while sys::clock() < until {
        nap();
    }
}

/// Every xHCI function in the device list, in list order (a desktop board
/// has a PCH controller and often a CPU-side or add-in one too).
pub(super) fn find_all() -> Result<Vec<Row>, Error> {
    let mut rows = vec![[0u64; dev::ROW_WORDS]; dev::MAX_ROWS];
    let mut total = dev::list(&mut rows).map_err(Error::Dev)?;
    if total > rows.len() {
        // A big machine: ask again with room for every function.
        rows = vec![[0u64; dev::ROW_WORDS]; total.min(1024)];
        total = dev::list(&mut rows).map_err(Error::Dev)?;
    }
    Ok(rows
        .iter()
        .take(total.min(rows.len()))
        .map(Row::from_words)
        .filter(|row| {
            row.flags & dev::row_flag::PCI != 0 && (row.class, row.subclass, row.prog_if) == CLASS
        })
        .collect())
}

impl Hc {
    /// Claim the controller in `row`, map BAR 0, take it from the BIOS,
    /// reset it, set up the DCBAA, the scratchpads, the command and event
    /// rings, start it and power its ports.
    pub(super) fn open(row: &Row, index: usize) -> Result<Hc, Error> {
        // BAR 0 must be a present memory BAR (32- or 64-bit, any address).
        if row.bar_meta[0] & 0b11 != 0b01 || row.bar_len[0] < 0x1000 {
            return Err(Error::Bar);
        }
        let (handle, irq) = super::irq::claim(row)?;
        let command = dev::cfg_read(handle, PCI_COMMAND, 2).map_err(Error::Dev)?;
        dev::cfg_write(
            handle,
            PCI_COMMAND,
            2,
            u64::from(command) | PCI_MEMORY | PCI_BUS_MASTER,
        )
        .map_err(Error::Dev)?;
        let base = dev::map_bar(handle, 0).map_err(Error::Dev)?;
        let bar_len = row.bar_len[0] as usize;
        // SAFETY: the kernel mapped all of BAR 0 for the claim, which lives
        // until this task exits.
        let mut bar = unsafe { Bar::new(base, bar_len) };
        // Before any other register write: firmware with legacy USB
        // emulation drives the controller from SMM until it lets go.
        let hccparams1 = bar.read32(cap::HCCPARAMS1);
        let first = regs::extended_caps(hccparams1);
        let deadline = sys::clock() + HANDOFF_TICKS;
        let handoff = extcap::legacy_handoff(&mut bar, first, bar_len, || {
            nap();
            sys::clock() <= deadline
        });
        let ports = Ports::read(&bar, first, bar_len);
        let core = Region::alloc(handle, CORE_BYTES)?;
        let commands =
            ProducerRing::new(core.ring(COMMAND_RING, COMMAND_TRBS)).map_err(Error::Xhci)?;
        let events = EventRing::new(core.ring(EVENT_RING, EVENT_TRBS)).map_err(Error::Xhci)?;
        let capbase = bar.read32(cap::CAPLENGTH);
        let mut hc = Hc {
            index,
            handle,
            op: (capbase & 0xFF) as usize,
            rt: (bar.read32(cap::RTSOFF) & !0x1F) as usize,
            db: bar.read32(cap::DBOFF),
            info: Info {
                version: (capbase >> 16) as u16,
                ports: 0,
                slots: 0,
                scratchpads: 0,
                context_64: regs::context_64(hccparams1),
                addressing_64: regs::addressing_64(hccparams1),
                port_power: regs::port_power_control(hccparams1),
                handoff,
            },
            ports,
            bar,
            core,
            scratchpad: None,
            commands,
            events,
            pending: VecDeque::new(),
            dropped: 0,
            pool: [const { None }; MAX_SLOTS as usize + 1],
            regions: 0,
            bulk: None,
            irq: None,
        };
        hc.reset()?;
        hc.configure()?;
        hc.start()?;
        super::irq::arm(&mut hc, irq);
        Ok(hc)
    }

    /// `USBCMD`/`USBSTS` in one short string, for failure snapshots.
    pub(super) fn state_text(&self) -> alloc::string::String {
        alloc::format!(
            "usbcmd={:#x} usbsts={:#x}",
            self.opreg(op::USBCMD),
            self.opreg(op::USBSTS)
        )
    }

    fn opreg(&self, offset: usize) -> u32 {
        self.bar.read32(self.op + offset)
    }

    fn set_opreg(&mut self, offset: usize, value: u32) {
        self.bar.write32(self.op + offset, value);
    }

    /// Wait up to `ticks` until `done` holds, napping between reads.
    fn until(
        &self,
        what: &'static str,
        ticks: u64,
        done: impl Fn(&Hc) -> bool,
    ) -> Result<(), Error> {
        let deadline = sys::clock() + ticks;
        while !done(self) {
            if sys::clock() > deadline {
                return Err(Error::Timeout(what));
            }
            nap();
        }
        Ok(())
    }

    /// Halt (if running) and reset the controller (4.22.1, 5.4.1).
    fn reset(&mut self) -> Result<(), Error> {
        self.until("controller ready", RESET_TICKS, |hc| {
            hc.opreg(op::USBSTS) & op::STS_CNR == 0
        })?;
        if self.opreg(op::USBSTS) & op::STS_HALTED == 0 {
            let cmd = self.opreg(op::USBCMD);
            self.set_opreg(op::USBCMD, cmd & !op::CMD_RUN);
            self.until("halt", TIMEOUT_TICKS, |hc| {
                hc.opreg(op::USBSTS) & op::STS_HALTED != 0
            })?;
        }
        self.set_opreg(op::USBCMD, op::CMD_RESET);
        // Some Intel controllers hang if their registers are read within
        // 1 ms of HCRST (Linux's XHCI_INTEL_HOST delay).
        sleep_ms(1);
        self.until("reset", RESET_TICKS, |hc| {
            hc.opreg(op::USBCMD) & op::CMD_RESET == 0 && hc.opreg(op::USBSTS) & op::STS_CNR == 0
        })
    }

    /// Program slots, the DCBAA, scratchpads and both rings (xHCI 4.2).
    fn configure(&mut self) -> Result<(), Error> {
        let structural = Structural::decode(self.bar.read32(cap::HCSPARAMS1));
        self.info.ports = structural.max_ports;
        self.info.slots = structural.max_slots.min(MAX_SLOTS);
        self.info.scratchpads = regs::scratchpad_count(self.bar.read32(cap::HCSPARAMS2));
        // Only 4 KiB pages are supported (every DMA buffer is 4 KiB-aligned).
        if self.opreg(op::PAGESIZE) & 1 == 0 {
            return Err(Error::PageSize);
        }
        if self.info.slots == 0 || self.info.ports == 0 {
            return Err(Error::NoPorts);
        }
        // Every DMA buffer comes from below 4 GiB (no `ADDR64` flag), so a
        // controller without 64-bit addressing (`AC64` clear) is fine too.
        let config = self.opreg(op::CONFIG) & !0xFF;
        self.set_opreg(op::CONFIG, config | u32::from(self.info.slots));
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

    /// Hand the controller the scratchpad buffers it asked for (4.20): an
    /// array of page addresses (64-byte aligned) and the pages.
    fn scratchpads(&mut self) -> Result<(), Error> {
        let count = usize::from(self.info.scratchpads);
        if count == 0 {
            return Ok(());
        }
        // One page for the array (up to 512 entries), then one per buffer.
        let array_pages = (count * 8).div_ceil(PAGE);
        let mut pages = Region::alloc(self.handle, (count + array_pages) * PAGE)?;
        for index in 0..count {
            let buffer = pages.bus((index + array_pages) * PAGE);
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
        self.until("run", TIMEOUT_TICKS, |hc| {
            hc.opreg(op::USBSTS) & op::STS_HALTED == 0
        })?;
        // A No Op proves the command and event rings work end to end.
        self.command(trb::no_op_command()).map(|_| ())
    }

    /// `HSE` or `HCE`: the controller hit a fatal error and stopped.
    pub(super) fn failed(&self) -> bool {
        self.opreg(op::USBSTS) & (op::STS_HSE | op::STS_HCE) != 0
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

    /// The bulk window's region and the window's offset in it, allocated on
    /// first use: one DMA buffer per controller, however many sticks.
    pub(super) fn bulk(&mut self) -> Result<(&mut Region, usize), Error> {
        if self.bulk.is_none() {
            self.bulk = Some(Region::alloc(self.handle, 2 * BULK_WINDOW)?);
        }
        let region = self.bulk.as_mut().ok_or(Error::Descriptor("bulk buffer"))?;
        let bus = region.bus(0);
        let offset = (bus.next_multiple_of(BULK_WINDOW as u64) - bus) as usize;
        Ok((region, offset))
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
        let event = self
            .wait(|e| e.kind() == kind::COMMAND_COMPLETION && e.parameter == pointer)
            .map_err(|error| match error {
                // Say which command, so a hang on a real controller points at
                // its step (a bare "event" cannot).
                Error::Timeout(_) => Error::Timeout(super::names::command_name(command.kind())),
                other => other,
            })?;
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

    /// Drop every queued transfer event of `slot` (`dci` 0: all of its
    /// endpoints). Called after Disable Slot, or after an endpoint was reset
    /// and its ring skipped: the memory is reused at the same bus addresses,
    /// so a stale completion could otherwise look like a new one.
    pub(super) fn discard(&mut self, slot: u8, dci: u8) {
        self.pump();
        self.pending.retain(|event| {
            !(event.kind() == kind::TRANSFER_EVENT
                && event.slot() == slot
                && (dci == 0 || event.endpoint() == dci))
        });
    }

    /// Take the oldest pending event, if any.
    pub(super) fn next_event(&mut self) -> Option<Trb> {
        self.pump();
        self.pending.pop_front()
    }

    /// Interrupter 0 raises the interrupt: no moderation beyond `imod`
    /// (250 ns units), pending flag cleared, then the controller-wide enable.
    pub(super) fn enable_interrupter(&mut self, imod: u32) {
        let at = self.rt + rt::INTERRUPTERS;
        self.bar.write32(at + rt::IMOD, imod);
        self.bar.write32(at + rt::IMAN, rt::IMAN_IE | rt::IMAN_IP);
        let cmd = self.opreg(op::USBCMD);
        self.set_opreg(op::USBCMD, cmd | op::CMD_INTE);
    }

    /// Clear interrupter 0's pending flag and the status bit (both write 1
    /// to clear), so the line drops before the kernel unmasks it.
    pub(super) fn clear_interrupt(&mut self) {
        let at = self.rt + rt::INTERRUPTERS + rt::IMAN;
        self.bar.write32(at, rt::IMAN_IE | rt::IMAN_IP);
        self.set_opreg(op::USBSTS, op::STS_EINT);
    }

    /// `PORTSC` of `port` (1-based).
    pub(super) fn portsc(&self, port: u8) -> u32 {
        self.opreg(op::PORTS + usize::from(port - 1) * op::PORT_STRIDE)
    }

    pub(super) fn set_portsc(&mut self, port: u8, value: u32) {
        self.set_opreg(op::PORTS + usize::from(port - 1) * op::PORT_STRIDE, value);
    }
}
