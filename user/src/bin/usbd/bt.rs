//! The USB Bluetooth transport (docs/bluetooth-plan.md, phase B2; HCI over
//! USB, "H2"): a controller's interface 0 (class `E0/01/01`) bound and its
//! packets served to `btd` as `os.lazy.usb.bt` (`bt_service.rs`).
//!
//! The function knows no Bluetooth beyond what delimits a packet
//! (`libs/btusb`): HCI commands go out as class requests on endpoint 0,
//! events come in on the interrupt-IN endpoint and ACL data on the bulk
//! endpoints, each IN endpoint keeping several chunk-sized transfers queued
//! and cutting packets out of the chunks. Interface 1 (the isochronous voice
//! channel and its alternates) is left alone. Packets reach `btd` through the
//! rings it attached (`bt_link.rs`); sending is `bt_out.rs`.
//!
//! One controller at a time across every xHCI controller: a second is
//! reported (`USBD:BT:BUSY`) and left alone. Only built in with
//! `LAZYOS_BT=1` (`cfg(lazyos_bt)`); without it the class is not bound.
//!
//! Serial evidence: `USBD:BT:BIND` (a controller bound, with its endpoints),
//! `USBD:BT:UNBIND` (it went away), `USBD:BT:ATTACH` / `BT:DETACH` (`btd`'s
//! rings), `USBD:BT:SKIP`, `USBD:BT:BUSY`, `USBD:BT:FAIL`.

use alloc::boxed::Box;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use user::trace;

use btusb::{Kind, Reassembler, PACKET_ACL, PACKET_EVENT};
use framering::{FrameBuf, MAX_FRAME};
use usbhid::desc::{Endpoint, Interface, Transfer};
use user::messenger::usb_bt::wire;
use user::messenger::Error as MsgError;
use xhci::regs::Speed;
use xhci::trb::{self, code, request, Trb};

use super::bt_link::{Backlog, Link};
use super::device::Device;
use super::hc::Hc;
use super::pipe::{Pipe, PIPE_TRBS};
use super::Error;

/// The wireless-controller class, its RF subclass and the Bluetooth
/// programming interface (USB-IF class codes; Bluetooth Core, vol 4 part B).
pub(super) const CLASS_WIRELESS: u8 = 0xE0;
const SUBCLASS_RF: u8 = 0x01;
const PROTOCOL_BLUETOOTH: u8 = 0x01;

/// A Bluetooth controller is bound somewhere (one at a time).
static BOUND: AtomicBool = AtomicBool::new(false);
/// Counts the controllers bound since boot: the service tells a controller
/// that left and came back (Intel re-enumerates after a firmware reset) from
/// one that never left, even when no loop pass saw it unbound.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The most one IN chunk (a bulk endpoint's `wMaxPacketSize` at high speed)
/// and the buffer space of a pipe window (`pipe.rs`: ring, then buffers).
const MAX_CHUNK: usize = 512;
const BUFFER_BYTES: usize = super::pipe::PIPE_BYTES - PIPE_TRBS * 16;
/// IN transfers kept queued per endpoint when the chunk is small enough.
const DEPTH: usize = 8;
/// Failed transfers in a row after which the controller is given up.
const MAX_ERRORS: u32 = 3;

/// An IN endpoint (interrupt or bulk) with several chunk transfers queued
/// and the reassembly of packets out of the chunks.
struct InPipe {
    pipe: Pipe,
    /// Where the chunk buffers start in the device region.
    buffers: usize,
    chunk: usize,
    depth: usize,
    next: usize,
    errors: u32,
    reassembler: Box<Reassembler>,
    address: u8,
}

enum Taken {
    /// This many bytes of the chunk buffer.
    Data(usize),
    Failed(u8),
    Stale,
}

impl InPipe {
    fn new(pipe: Pipe, window: usize, endpoint: &Endpoint, kind: Kind) -> InPipe {
        let chunk = usize::from(endpoint.max_packet).clamp(1, MAX_CHUNK);
        InPipe {
            pipe,
            buffers: window + PIPE_TRBS * 16,
            chunk,
            depth: (BUFFER_BYTES / chunk).clamp(1, DEPTH),
            next: 0,
            errors: 0,
            reassembler: Box::new(Reassembler::new(kind)),
            address: endpoint.address,
        }
    }

    fn dci(&self) -> u8 {
        self.pipe.dci
    }

    /// Top the ring up to the queue depth and ring once. Transfers complete
    /// in ring order and at most `depth` are queued, so a chunk buffer is
    /// reused only after its report was read ([`InPipe::take`]).
    fn refill(&mut self, hc: &mut Hc, device: &Device) -> Result<(), Error> {
        let mut added = false;
        while self.pipe.in_flight() < self.depth {
            let at = self.buffers + self.next * self.chunk;
            self.pipe.submit(
                trb::interrupt_in(device.mem.bus(at), self.chunk as u16),
                self.next,
            )?;
            self.next = (self.next + 1) % self.depth;
            added = true;
        }
        if added {
            hc.doorbell(device.slot, self.pipe.dci);
        }
        Ok(())
    }

    fn take(&mut self, event: &Trb, device: &Device, out: &mut [u8; MAX_CHUNK]) -> Taken {
        let Some(done) = self.pipe.complete(event) else {
            return Taken::Stale;
        };
        if !matches!(done.code, code::SUCCESS | code::SHORT_PACKET) {
            self.errors += 1;
            return Taken::Failed(done.code);
        }
        self.errors = 0;
        let len = self.chunk - (done.residual as usize).min(self.chunk);
        device
            .mem
            .read(self.buffers + done.tag * self.chunk, &mut out[..len]);
        Taken::Data(len)
    }

    /// A failed transfer: bring the endpoint back (Reset Endpoint, a skipped
    /// ring, `CLEAR_FEATURE(ENDPOINT_HALT)` after a stall) and forget the
    /// half-received packet. `false` when it keeps failing.
    fn recover(&mut self, hc: &mut Hc, device: &mut Device, failed: u8) -> bool {
        self.reassembler.reset();
        if self.errors > MAX_ERRORS {
            return false;
        }
        let dci = self.dci();
        let pointer = self.pipe.abandon();
        if device.recover(hc, dci, pointer).is_err() {
            return false;
        }
        if failed == code::STALL {
            let _ = device.control_out(hc, request::clear_endpoint_halt(self.address));
        }
        true
    }
}

/// Where received packets go: `btd`'s rings, or the bounded backlog behind a
/// full ring.
pub(super) struct Sink {
    pub(super) link: Option<Link>,
    backlog: Backlog,
    /// Frames were queued since the last wake-up decision.
    wake: bool,
    pub(super) stats: wire::Stats,
}

impl Sink {
    /// Hand one packet (`kind` is the H4 indicator) to `btd`.
    fn deliver(&mut self, kind: u8, packet: &[u8]) {
        let mut buf = [0u8; MAX_FRAME];
        let Some(len) = btusb::frame(kind, packet, &mut buf) else {
            self.stats.oversize += 1;
            return;
        };
        if kind == PACKET_EVENT {
            self.stats.events += 1;
        } else {
            self.stats.acl_in += 1;
        }
        let frame = &buf[..len];
        let Some(link) = self.link.as_mut() else {
            self.stats.dropped += 1;
            return;
        };
        if self.backlog.is_empty() {
            match link.push(frame) {
                super::bt_link::Pushed::Queued => {
                    self.wake = true;
                    return;
                }
                super::bt_link::Pushed::Poisoned => return self.poison(),
                super::bt_link::Pushed::Full => {}
            }
        }
        if !self.backlog.push(frame) {
            self.stats.dropped += 1;
        }
    }

    /// After a burst (and on every serve pass): move the backlog into freed
    /// room and send the doorbell if `btd` is asleep on the ring.
    pub(super) fn finish(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        if !self.backlog.is_empty() {
            match self.backlog.drain_into(link) {
                Ok(moved) => self.wake |= moved,
                Err(()) => return self.poison(),
            }
        }
        if core::mem::take(&mut self.wake)
            && link.wants_notify()
            && !link.notify(1 << wire::NOTIFY_BIT_PACKETS)
        {
            self.drop_link("btd is gone");
        }
    }

    pub(super) fn poison(&mut self) {
        self.stats.ring_errors += 1;
        self.drop_link("ring corrupt");
    }

    pub(super) fn drop_link(&mut self, why: &str) {
        self.backlog.clear();
        self.wake = false;
        if let Some(link) = self.link.take() {
            link.close(why);
        }
    }
}

pub(super) struct BtFn {
    pub(super) interface: u8,
    /// Which bind this is (see [`GENERATION`]).
    pub(super) generation: u64,
    intr: InPipe,
    bulk_in: InPipe,
    pub(super) bulk_out: Pipe,
    info: wire::Device,
    pub(super) sink: Sink,
    next_ring: u32,
    /// Where a command frame from `btd` is copied before it is looked at.
    pub(super) scratch: Box<FrameBuf>,
}

impl BtFn {
    /// The Bluetooth function on interface 0, if the device has one with its
    /// three endpoints and no other controller is bound.
    pub(super) fn bind(
        hc: &mut Hc,
        device: &mut Device,
        interface: &Interface,
    ) -> Result<Option<BtFn>, Error> {
        let name = device.name.clone();
        if interface.number != 0
            || interface.subclass != SUBCLASS_RF
            || interface.protocol != PROTOCOL_BLUETOOTH
        {
            return Ok(None);
        }
        let endpoints = (
            interface.endpoint(Transfer::Interrupt, true),
            interface.endpoint(Transfer::Bulk, true),
            interface.endpoint(Transfer::Bulk, false),
        );
        let (Some(intr), Some(bulk_in), Some(bulk_out)) = endpoints else {
            trace::line!(
                trace::Channel::Usbd,
                "BT:SKIP port={name} no interrupt-IN and bulk endpoints"
            );
            return Ok(None);
        };
        // A chunk is one packet of the endpoint: a SuperSpeed bulk endpoint
        // (1024) does not fit a pipe window's buffers.
        let too_big = [intr, bulk_in, bulk_out]
            .iter()
            .any(|e| usize::from(e.max_packet) > MAX_CHUNK);
        if too_big || matches!(device.at.speed, Speed::Super | Speed::SuperPlus) {
            trace::line!(
                trace::Channel::Usbd,
                "BT:SKIP port={name} SuperSpeed controllers are not supported"
            );
            return Ok(None);
        }
        if BOUND.load(Ordering::Acquire) {
            trace::line!(
                trace::Channel::Usbd,
                "BT:BUSY port={name} another controller is in use"
            );
            return Ok(None);
        }
        let product = device.product_name(hc);
        let (intr_pipe, intr_window) = device.open_window(&intr)?;
        let (in_pipe, in_window) = device.open_window(&bulk_in)?;
        let out_pipe = device.open_window(&bulk_out)?.0;
        // From here `BtFn`'s drop gives the claim back. (Single-threaded: no
        // other bind runs between the check above and this.)
        BOUND.store(true, Ordering::Release);
        let function = BtFn {
            interface: interface.number,
            generation: GENERATION.fetch_add(1, Ordering::AcqRel) + 1,
            intr: InPipe::new(intr_pipe, intr_window, &intr, Kind::Event),
            bulk_in: InPipe::new(in_pipe, in_window, &bulk_in, Kind::Acl),
            bulk_out: out_pipe,
            info: wire::Device {
                vendor: u32::from(device.descriptor.vendor),
                product: u32::from(device.descriptor.product),
                version: u32::from(device.descriptor.version),
                name: product,
                port: String::from(device.name.as_str()),
                high_speed: matches!(device.at.speed, Speed::High),
            },
            sink: Sink {
                link: None,
                backlog: Backlog::default(),
                wake: false,
                stats: wire::Stats::default(),
            },
            next_ring: 1,
            scratch: Box::new([0; MAX_FRAME]),
        };
        trace::line!(trace::Channel::Usbd, "BT:BIND port={} slot={} vendor={:#06x} product={:#06x} interface={} intr={} bulk_in={} bulk_out={} chunk={}",
            device.name,
            device.slot,
            device.descriptor.vendor,
            device.descriptor.product,
            function.interface,
            function.intr.dci(),
            function.bulk_in.dci(),
            function.bulk_out.dci,
            function.bulk_in.chunk);
        Ok(Some(function))
    }

    /// After Configure Endpoint: queue the IN transfers. `false` when that
    /// fails (reported); the caller drops the function.
    pub(super) fn start(&mut self, hc: &mut Hc, device: &mut Device) -> bool {
        let started = self
            .intr
            .refill(hc, device)
            .and_then(|()| self.bulk_in.refill(hc, device));
        if let Err(error) = started {
            trace::line!(
                trace::Channel::Usbd,
                "BT:FAIL port={} start: {error}",
                device.name
            );
            return false;
        }
        true
    }

    /// Whether the transfer events of endpoint `dci` are this function's.
    pub(super) fn owns(&self, dci: u8) -> bool {
        self.intr.dci() == dci || self.bulk_in.dci() == dci || self.bulk_out.dci == dci
    }

    /// A transfer event of an IN endpoint: cut its packets out of the chunk,
    /// queue the next transfer. (The bulk-OUT endpoint is driven
    /// synchronously by `bt_out.rs`; a late event of it is stale.) An error
    /// is a controller that keeps failing: the caller detaches it.
    pub(super) fn on_transfer(
        &mut self,
        hc: &mut Hc,
        device: &mut Device,
        event: &Trb,
    ) -> Result<(), Error> {
        let dci = event.endpoint();
        let BtFn {
            intr,
            bulk_in,
            sink,
            ..
        } = self;
        let (pipe, kind) = if dci == intr.dci() {
            (intr, PACKET_EVENT)
        } else if dci == bulk_in.dci() {
            (bulk_in, PACKET_ACL)
        } else {
            return Ok(());
        };
        let mut chunk = [0u8; MAX_CHUNK];
        match pipe.take(event, device, &mut chunk) {
            Taken::Stale => return Ok(()),
            Taken::Data(len) => pipe
                .reassembler
                .push(&chunk[..len], |packet| sink.deliver(kind, packet)),
            Taken::Failed(failed) => {
                sink.stats.errors += 1;
                if !pipe.recover(hc, device, failed) {
                    return Err(Error::Completion(0, failed));
                }
            }
        }
        pipe.refill(hc, device)?;
        sink.finish();
        Ok(())
    }

    pub(super) fn describe(&self) -> wire::Device {
        self.info.clone()
    }

    pub(super) fn stats(&self) -> wire::Stats {
        let mut stats = self.sink.stats.clone();
        stats.oversize += u64::from(self.intr.reassembler.oversize())
            + u64::from(self.bulk_in.reassembler.oversize());
        stats
    }

    /// `AttachRing`: take `btd`'s rings. An earlier attachment whose `btd`
    /// is gone (it crashed and was restarted) is replaced; a live one is
    /// `EBUSY`. The decoded request owns the buffer and the notify endpoint;
    /// the caller closes them on `Err`.
    pub(super) fn attach(
        &mut self,
        owner: u64,
        args: &wire::AttachRingArgs,
        notify: user::messenger::Endpoint,
    ) -> Result<u32, MsgError> {
        if self.sink.link.as_ref().is_some_and(|old| old.notify(0)) {
            return Err(MsgError::Errno(-user::messenger::errno::EBUSY));
        }
        self.sink.drop_link("replaced by a new attachment");
        let ring = self.next_ring;
        let link = Link::attach(owner, ring, args, notify)?;
        self.next_ring = self.next_ring.wrapping_add(1).max(1);
        trace::line!(
            trace::Channel::Usbd,
            "BT:ATTACH owner={owner} ring={ring} slots={}",
            args.slots
        );
        self.sink.link = Some(link);
        Ok(ring)
    }

    /// The device is going away: tell `btd`, then release everything.
    pub(super) fn close(mut self) {
        if let Some(link) = self.sink.link.as_ref() {
            link.notify(1 << wire::NOTIFY_BIT_GONE);
        }
        trace::line!(trace::Channel::Usbd, "BT:UNBIND port={}", self.info.port);
        self.sink.drop_link("controller unplugged");
    }
}

impl Drop for BtFn {
    fn drop(&mut self) {
        self.sink.drop_link("function dropped");
        BOUND.store(false, Ordering::Release);
    }
}
