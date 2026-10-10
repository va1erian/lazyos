//! One USB device, whatever its class: a slot, endpoint 0 and the pipes its
//! interfaces asked for (xHCI 4.3, 4.6.5, 4.6.6; USB 2.0 9.1.2, 9.4).
//!
//! [`Device::enable`] addresses it at its [`Location`] (root port, or hub
//! port with route string and transaction translator), reads its device and
//! configuration descriptors, and leaves the rest to the class drivers
//! (`class.rs`): they send their requests through [`Device::control_in`] /
//! [`Device::control_out`], open pipes with [`Device::open_reports`] or
//! [`Device::open_pipe`], and [`Device::configure`] gives every pipe to the
//! controller in one Configure Endpoint.
//!
//! Everything the device returns is parsed from a copy with `libs/usbhid`;
//! a device that answers nonsense is reported and left unconfigured. A
//! request it stalls is survivable: endpoint 0 is reset and its ring skipped
//! past the failed transfer, so the next request works.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use usbhid::desc::{self, Config, DeviceDescriptor, Endpoint};
use user::sys;
use xhci::context::{slot_state, EndpointContext, HubSlot, InputContext, INPUT_CONTEXTS};
use xhci::regs::Speed;
use xhci::ring::{ProducerRing, RawMem};
use xhci::route::Location;
use xhci::trb::{self, code, kind, request, SetupPacket, Trb};

use super::hc::{sleep_ms, Hc};
use super::mem::{Region, PAGE};
use super::pipe::{Pipe, Report, Reports, MAX_REPORT, PIPE_BYTES};
use super::Error;

/// TRBs in endpoint 0's ring.
const EP0_TRBS: usize = 32;
/// Layout of a device's region. Page 0: the input context (up to 33 * 64
/// bytes with 64-byte contexts) and endpoint 0's ring. Page 1: the output
/// device context (up to 32 * 64 bytes) and the control data buffer. Page
/// 2: one [`PIPE_BYTES`] window per pipe.
const INPUT: usize = 0;
const EP0_RING: usize = 3072;
const OUTPUT: usize = PAGE;
const DATA: usize = PAGE + 2048;
pub(super) const DATA_BYTES: usize = 1024;
const PIPES: usize = 2 * PAGE;
/// Pipes one device may open (one per interface it binds, two for bulk).
pub(super) const MAX_PIPES: usize = 4;
pub(super) const DEVICE_BYTES: usize = PIPES + MAX_PIPES * PIPE_BYTES;
const _: () = assert!(INPUT + INPUT_CONTEXTS * 64 <= EP0_RING);
const _: () = assert!(EP0_RING + EP0_TRBS * 16 <= OUTPUT);
const _: () = assert!(OUTPUT + 32 * 64 <= DATA && DATA + DATA_BYTES <= PIPES);
const _: () = assert!(DEVICE_BYTES <= 3 * PAGE);
/// SET_ADDRESS recovery (USB 2.0 9.2.6.3 allows 2 ms; Linux waits 10).
const SET_ADDRESS_RECOVERY_MS: u64 = 10;

/// An addressed device and the pipes its class drivers opened.
pub(super) struct Device {
    pub(super) slot: u8,
    pub(super) at: Location,
    pub(super) descriptor: DeviceDescriptor,
    /// `hc=` and the location, as the markers name the device.
    pub(super) name: String,
    pub(super) mem: Region,
    ep0: ProducerRing<RawMem>,
    max_packet0: u16,
    /// Interrupt-IN pipes `usbd` refills itself (HID, hub status).
    reports: Vec<Reports>,
    /// Endpoint contexts of every pipe, given to Configure Endpoint.
    contexts: Vec<(u8, EndpointContext)>,
    windows_used: usize,
}

impl Device {
    /// Enable a slot for the device at `at`, address it and read its device
    /// descriptor. Every failure gives the slot and its memory back.
    pub(super) fn enable(hc: &mut Hc, at: Location) -> Result<Device, Error> {
        let slot = hc.command(trb::enable_slot())?.slot();
        if slot == 0 {
            return Err(Error::Completion(kind::ENABLE_SLOT, 0));
        }
        let mem = match hc.take_region(slot, DEVICE_BYTES) {
            Ok(mem) => mem,
            Err(error) => {
                let _ = hc.command(trb::disable_slot(slot));
                return Err(error);
            }
        };
        let ep0 = ProducerRing::new(mem.ring(EP0_RING, EP0_TRBS)).map_err(Error::Xhci)?;
        let mut device = Device {
            slot,
            at,
            // Read for real by `read_device` before anyone looks.
            descriptor: DeviceDescriptor {
                usb: 0,
                class: 0,
                subclass: 0,
                protocol: 0,
                max_packet0: 0,
                vendor: 0,
                product: 0,
                configurations: 0,
            },
            name: format!("{}-{}", hc.index, at),
            mem,
            ep0,
            max_packet0: at.speed.default_max_packet0(),
            reports: Vec::new(),
            contexts: Vec::new(),
            windows_used: 0,
        };
        match device.address(hc).and_then(|()| device.read_device(hc)) {
            Ok(()) => Ok(device),
            Err(error) => {
                device.release(hc);
                Err(error)
            }
        }
    }

    /// Address Device: the slot context names the device's place in the
    /// tree (route string, root port, transaction translator).
    fn address(&mut self, hc: &mut Hc) -> Result<(), Error> {
        hc.set_device_context(self.slot, self.mem.bus(OUTPUT));
        let ep0 = EndpointContext::control(self.max_packet0, self.ep0.dequeue_pointer());
        let slot = self.at.slot_context(1, None);
        let mut input = self.input(hc)?;
        input.slot(&slot).map_err(Error::Xhci)?;
        input.endpoint(1, &ep0).map_err(Error::Xhci)?;
        hc.command(trb::address_device(self.mem.bus(INPUT), self.slot, false))?;
        sleep_ms(SET_ADDRESS_RECOVERY_MS);
        Ok(())
    }

    /// The device descriptor, fixing endpoint 0's packet size first (full
    /// speed devices may use 8, 16, 32 or 64; USB 3 encodes a power of two).
    fn read_device(&mut self, hc: &mut Hc) -> Result<(), Error> {
        let mut head = [0u8; 8];
        let setup = request::get_descriptor(desc::kind::DEVICE, 0, 8);
        self.control_in(hc, setup, &mut head)?;
        let max_packet0 = match (self.at.speed, head[7]) {
            (Speed::Super | Speed::SuperPlus, 9) => 512,
            (Speed::Low, 8) => 8,
            (Speed::Full | Speed::High, size @ (8 | 16 | 32 | 64)) => u16::from(size),
            _ => return Err(Error::Descriptor("bMaxPacketSize0")),
        };
        if max_packet0 != self.max_packet0 {
            let mut input = self.input(hc)?;
            input.ep0_max_packet(max_packet0).map_err(Error::Xhci)?;
            hc.command(trb::evaluate_context(self.mem.bus(INPUT), self.slot))?;
            self.max_packet0 = max_packet0;
        }
        let mut bytes = [0u8; desc::DEVICE_LEN];
        let setup = request::get_descriptor(desc::kind::DEVICE, 0, desc::DEVICE_LEN as u16);
        self.control_in(hc, setup, &mut bytes)?;
        self.descriptor = desc::parse_device(&bytes).map_err(|_| Error::Descriptor("device"))?;
        sys::write_str(&format!(
            "USBD:DESC:DEVICE port={} {}\n",
            self.name,
            hex(&bytes)
        ));
        Ok(())
    }

    /// The whole first configuration descriptor chain.
    pub(super) fn read_config(&mut self, hc: &mut Hc) -> Result<Config, Error> {
        let mut header = [0u8; desc::CONFIG_LEN];
        let setup = request::get_descriptor(desc::kind::CONFIGURATION, 0, header.len() as u16);
        self.control_in(hc, setup, &mut header)?;
        let total = desc::config_total_len(&header).map_err(|_| Error::Descriptor("config"))?;
        let mut chain = [0u8; DATA_BYTES];
        let chain = chain
            .get_mut(..usize::from(total))
            .ok_or(Error::Descriptor("wTotalLength"))?;
        let setup = request::get_descriptor(desc::kind::CONFIGURATION, 0, total);
        self.control_in(hc, setup, chain)?;
        sys::write_str(&format!(
            "USBD:DESC:CONFIG port={} {}\n",
            self.name,
            hex(chain)
        ));
        desc::parse_config(chain).map_err(|_| Error::Descriptor("config chain"))
    }

    /// An interrupt-IN pipe to `endpoint` whose reports `usbd` collects,
    /// `depth` transfers deep. Returns its DCI.
    pub(super) fn open_reports(&mut self, endpoint: &Endpoint, depth: usize) -> Result<u8, Error> {
        let (pipe, window) = self.open_window(endpoint)?;
        let dci = pipe.dci;
        self.reports.push(Reports::new(pipe, window, depth));
        Ok(dci)
    }

    /// A pipe to `endpoint` that the calling class drives itself (bulk):
    /// it submits transfers into buffers of its own and gets the events of
    /// its DCI from the dispatcher. Its context is configured with the rest.
    /// Mass storage (`msc.rs`) uses it.
    pub(super) fn open_pipe(&mut self, endpoint: &Endpoint) -> Result<Pipe, Error> {
        self.open_window(endpoint).map(|(pipe, _)| pipe)
    }

    fn open_window(&mut self, endpoint: &Endpoint) -> Result<(Pipe, usize), Error> {
        if self.windows_used == MAX_PIPES {
            return Err(Error::Descriptor("too many endpoints"));
        }
        let window = PIPES + self.windows_used * PIPE_BYTES;
        let pipe = Pipe::new(
            self.mem.ring(window, super::pipe::PIPE_TRBS),
            endpoint,
            self.at.speed,
        )?;
        if self.contexts.iter().any(|&(dci, _)| dci == pipe.dci) {
            return Err(Error::Descriptor("endpoint listed twice"));
        }
        self.windows_used += 1;
        self.contexts.push((pipe.dci, pipe.context));
        Ok((pipe, window))
    }

    /// Configure Endpoint with every pipe opened so far, declaring the
    /// device a hub when `hub` is set; then start the report pipes.
    pub(super) fn configure(&mut self, hc: &mut Hc, hub: Option<HubSlot>) -> Result<(), Error> {
        let entries = self.contexts.iter().map(|&(dci, _)| dci).max().unwrap_or(1);
        let slot = self.at.slot_context(entries, hub);
        let contexts = self.contexts.clone();
        let mut input = self.input(hc)?;
        input.slot(&slot).map_err(Error::Xhci)?;
        for (dci, context) in &contexts {
            input.endpoint(*dci, context).map_err(Error::Xhci)?;
        }
        hc.command(trb::configure_endpoint(self.mem.bus(INPUT), self.slot))?;
        for reports in &mut self.reports {
            reports.refill(hc, &self.mem, self.slot)?;
        }
        Ok(())
    }

    /// Whether `event` belongs to one of this device's report pipes.
    pub(super) fn has_reports(&self, dci: u8) -> bool {
        self.reports.iter().any(|r| r.dci() == dci)
    }

    /// Take a completed report of pipe `dci` into `out` and queue the next
    /// transfer. A halted pipe is reset and restarted; one failing many
    /// times in a row is reported as failed for the caller to give up on.
    pub(super) fn take_report(
        &mut self,
        hc: &mut Hc,
        event: &Trb,
        out: &mut [u8; MAX_REPORT],
    ) -> Report {
        let Some(index) = self
            .reports
            .iter()
            .position(|r| r.dci() == event.endpoint())
        else {
            return Report::Stale;
        };
        let report = self.reports[index].take(event, &self.mem, out);
        if let Report::Failed(code) = report {
            if self.reports[index].errors > 3 {
                return report;
            }
            let dci = self.reports[index].dci();
            let pointer = self.reports[index].pipe.abandon();
            if self.recover(hc, dci, pointer).is_err() {
                return report;
            }
            // A STALL is the device halting its endpoint: clear that too.
            if code == code::STALL {
                let address = (dci / 2) | (dci & 1) << 7;
                let _ = self.control_out(hc, request::clear_endpoint_halt(address));
            }
        }
        if self.reports[index]
            .refill(hc, &self.mem, self.slot)
            .is_err()
        {
            return Report::Failed(0);
        }
        report
    }

    /// Give the slot back: Disable Slot stops every endpoint and the
    /// controller lets go of the contexts and rings, after which the memory
    /// can serve the slot's next device. If the command fails the
    /// controller may still own the memory, so it is never reused.
    pub(super) fn release(self, hc: &mut Hc) {
        let slot = self.slot;
        match hc.command(trb::disable_slot(slot)) {
            Ok(_) => {
                hc.discard(slot, 0);
                hc.set_device_context(slot, 0);
                hc.give_region(slot, self.mem);
            }
            Err(error) => sys::write_str(&format!(
                "USBD:SLOT:LEAK slot={slot} disable failed: {error}\n"
            )),
        }
    }

    /// A control transfer with a device-to-host data stage into `out`.
    pub(super) fn control_in(
        &mut self,
        hc: &mut Hc,
        setup: SetupPacket,
        out: &mut [u8],
    ) -> Result<(), Error> {
        self.mem.clear(DATA, out.len().min(DATA_BYTES));
        self.control(hc, &setup)?;
        self.mem.read(DATA, out);
        Ok(())
    }

    pub(super) fn control_out(&mut self, hc: &mut Hc, setup: SetupPacket) -> Result<(), Error> {
        self.control(hc, &setup)
    }

    fn control(&mut self, hc: &mut Hc, setup: &SetupPacket) -> Result<(), Error> {
        if usize::from(setup.length) > DATA_BYTES {
            return Err(Error::Descriptor("request too long"));
        }
        let (trbs, count) = trb::control_transfer(setup, self.mem.bus(DATA));
        // No Chain bits: a control TD's stages are told apart by type, and a
        // Setup Stage TRB's bit 4 is reserved while a Data Stage TRB with it
        // set pulls the Status Stage into the data stage (xHCI 6.4.1.2). QEMU
        // ignores it; a real controller hangs or stalls (the Kaby Lake box).
        let status = self
            .ep0
            .enqueue(&trbs[..count], false)
            .map_err(Error::Xhci)?;
        hc.doorbell(self.slot, 1);
        let slot = self.slot;
        let waited =
            hc.wait(|e| e.kind() == kind::TRANSFER_EVENT && e.slot() == slot && e.endpoint() == 1);
        let failed = match waited {
            Ok(event) if event.parameter == status && event.completion_code() == code::SUCCESS => {
                return self.ep0.retire(status).map_err(Error::Xhci);
            }
            Ok(event) => Error::Completion(setup.request, event.completion_code()),
            Err(Error::Timeout(_)) => Error::Timeout(super::names::request_name(setup.request)),
            Err(error) => error,
        };
        // What the controller made of it, before recovery changes anything:
        // on a real PC "timed out" alone cannot say whether the controller
        // never fetched the TRBs or ran them and lost the event.
        self.snapshot(hc, &failed);
        // A stall (or a timeout) leaves endpoint 0 halted or busy with the
        // rest of the transfer: reset it and skip what is left, so the next
        // request starts clean.
        let pointer = self.ep0.abandon();
        self.recover(hc, 1, pointer)?;
        Err(failed)
    }

    /// One `USBD:DUMP:DEV` line: who is in the slot, the slot state and
    /// address the controller recorded and endpoint 0's state and
    /// dequeue pointer against our ring's.
    pub(super) fn dump_line(&mut self, hc: &Hc) -> String {
        let stride = if hc.info.context_64 { 16 } else { 8 };
        let ours = self.ep0.dequeue_pointer();
        let words = self.mem.dwords(OUTPUT, 2 * stride);
        let slot = match slot_state(words) {
            Some((state, address)) => format!("{state:?}/addr{address}"),
            None => String::from("?"),
        };
        let (ep_dw0, ep_dq) = (
            words[stride],
            u64::from(words[stride + 2]) | u64::from(words[stride + 3]) << 32,
        );
        format!(
            "USBD:DUMP:DEV hc={} port={} slot={} state={slot} ep0state={} ep0dq={ep_dq:#x} ring={ours:#x} mps={} vendor={:#06x} product={:#06x}",
            hc.index,
            self.name,
            self.slot,
            ep_dw0 & 7,
            self.max_packet0,
            self.descriptor.vendor,
            self.descriptor.product,
        )
    }

    /// One `USBD:DIAG` line: the slot state and address the controller
    /// recorded, endpoint 0's state and the dequeue pointer it has reached
    /// (against the ring's base `ours`), and the controller's own status.
    fn snapshot(&mut self, hc: &Hc, failed: &Error) {
        let stride = if hc.info.context_64 { 16 } else { 8 };
        let ours = self.ep0.dequeue_pointer();
        let words = self.mem.dwords(OUTPUT, 2 * stride);
        let slot = match slot_state(words) {
            Some((state, address)) => format!("{state:?}/addr{address}"),
            None => String::from("?"),
        };
        let (ep_dw0, ep_dq) = (
            words[stride],
            u64::from(words[stride + 2]) | u64::from(words[stride + 3]) << 32,
        );
        sys::write_str(&format!(
            "USBD:DIAG port={} {failed} slot={slot} ep0state={} ep0dq={ep_dq:#x} ring={ours:#x} mps={} {}\n",
            self.name,
            ep_dw0 & 7,
            self.max_packet0,
            hc.state_text()
        ));
    }

    /// Endpoint `dci`'s state as the controller recorded it (xHCI 6.2.3: 1
    /// running, 2 halted, 3 stopped, 4 error) and its dequeue pointer.
    pub(super) fn endpoint_state(&mut self, hc: &Hc, dci: u8) -> (u32, u64) {
        let stride = if hc.info.context_64 { 16 } else { 8 };
        let at = usize::from(dci) * stride;
        let words = self.mem.dwords(OUTPUT, at + stride);
        let dequeue = u64::from(words[at + 2]) | u64::from(words[at + 3]) << 32;
        (words[at] & 7, dequeue & !0xF)
    }

    /// Bring endpoint `dci` back after a failure: Reset Endpoint (a halted
    /// one) or Stop Endpoint (one still running), then point it past
    /// everything queued, and drop the stale events.
    pub(super) fn recover(&mut self, hc: &mut Hc, dci: u8, pointer: u64) -> Result<(), Error> {
        if let Err(Error::Completion(_, code::CONTEXT_STATE)) =
            hc.command(trb::reset_endpoint(self.slot, dci))
        {
            // Not halted: stop it instead (an already stopped one fails
            // the same way, which is fine).
            let _ = hc.command(trb::stop_endpoint(self.slot, dci));
        }
        hc.command(trb::set_tr_dequeue(self.slot, dci, pointer))?;
        hc.discard(self.slot, dci);
        Ok(())
    }

    /// Lay a fresh input context over the device region's first page.
    fn input(&mut self, hc: &Hc) -> Result<InputContext<'_>, Error> {
        let context_64 = hc.info.context_64;
        let stride = if context_64 { 16 } else { 8 };
        InputContext::new(self.mem.dwords(INPUT, INPUT_CONTEXTS * stride), context_64)
            .map_err(Error::Xhci)
    }
}

/// Lower-case hex of `bytes`, for the descriptor evidence lines.
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
