//! Pipes to endpoints other than endpoint 0 (xHCI 4.11, 4.14).
//!
//! A [`Pipe`] is one transfer ring and its endpoint context: what any class
//! needs, interrupt or bulk. [`Reports`] is the interrupt-IN pipe HID and
//! hubs use: a fixed set of report buffers, several transfers kept queued,
//! each completion handed out as a copy of its bytes.
//!
//! Each pipe takes one [`PIPE_BYTES`] window of its device's DMA region,
//! so a device's memory stays bounded however many interfaces it has.

use alloc::collections::VecDeque;

use usbhid::desc::{Endpoint, Transfer};
use xhci::context::{EndpointContext, EndpointType};
use xhci::regs::{self, Speed};
use xhci::ring::{ProducerRing, RawMem};
use xhci::trb::{self, code, Trb};

use super::hc::Hc;
use super::mem::Region;
use super::Error;

/// TRBs per pipe ring.
pub(super) const PIPE_TRBS: usize = 32;
/// Bytes of the device region one pipe takes: its ring, then its buffers.
pub(super) const PIPE_BYTES: usize = 1024;
const BUFFERS: usize = PIPE_TRBS * 16;
/// Largest report read (boot reports are 8 bytes, hub bitmaps 1 or 2).
pub(super) const MAX_REPORT: usize = 64;
/// Interrupt-IN transfers kept queued, each into a report buffer of its own.
/// With one transfer in flight a report cost a whole driver round trip (the
/// next poll, then a doorbell), and QEMU's `usb-kbd` hands out one keycode
/// per report, so fast typing overran its 16-entry queue (issue #480). With
/// several queued the controller completes one per endpoint interval on its
/// own and the driver drains them in batches.
pub(super) const REPORTS: usize = 8;
const _: () = assert!(BUFFERS + REPORTS * MAX_REPORT <= PIPE_BYTES);
const _: () = assert!(REPORTS < PIPE_TRBS - 2);

/// One endpoint's transfer ring and context.
pub(super) struct Pipe {
    pub(super) dci: u8,
    pub(super) context: EndpointContext,
    ring: ProducerRing<RawMem>,
    /// TRBs in flight, oldest first: bus address and the caller's tag.
    in_flight: VecDeque<(u64, usize)>,
}

/// A completed transfer: the tag it was submitted with, its completion
/// code and the bytes not transferred.
pub(super) struct Done {
    pub(super) tag: usize,
    pub(super) code: u8,
    pub(super) residual: u32,
}

impl Pipe {
    /// A pipe to `endpoint` (of a device running at `speed`) over `ring`.
    pub(super) fn new(ring: RawMem, endpoint: &Endpoint, speed: Speed) -> Result<Pipe, Error> {
        let ring = ProducerRing::new(ring).map_err(Error::Xhci)?;
        let dequeue = ring.dequeue_pointer();
        let usb3 = matches!(speed, Speed::Super | Speed::SuperPlus);
        let burst = if usb3 {
            endpoint.max_burst
        } else {
            endpoint.extra
        };
        let max_packet = endpoint.max_packet.clamp(1, 1024);
        let context = match (endpoint.transfer(), endpoint.is_in()) {
            (Transfer::Bulk, is_in) => {
                let kind = if is_in {
                    EndpointType::BulkIn
                } else {
                    EndpointType::BulkOut
                };
                EndpointContext::bulk(kind, max_packet, burst, dequeue)
            }
            (Transfer::Interrupt, is_in) => {
                let kind = if is_in {
                    EndpointType::InterruptIn
                } else {
                    EndpointType::InterruptOut
                };
                let interval = regs::interrupt_interval(speed, endpoint.interval);
                let mut context =
                    EndpointContext::interrupt(kind, max_packet, burst, interval, dequeue);
                if usb3 && endpoint.bytes_per_interval != 0 {
                    context.max_esit = u32::from(endpoint.bytes_per_interval);
                }
                context
            }
            _ => return Err(Error::Descriptor("isochronous or control endpoint")),
        };
        Ok(Pipe {
            dci: regs::dci(endpoint.address),
            context,
            ring,
            in_flight: VecDeque::new(),
        })
    }

    /// Put one transfer TRB on the ring (the caller rings the doorbell).
    pub(super) fn submit(&mut self, transfer: Trb, tag: usize) -> Result<(), Error> {
        let pointer = self.ring.enqueue(&[transfer], false).map_err(Error::Xhci)?;
        self.in_flight.push_back((pointer, tag));
        Ok(())
    }

    pub(super) fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Match a transfer event to the TRB it names. Transfers complete in
    /// ring order, so older ones still listed lost their event (dropped
    /// under load) and are forgotten. `None` for a pointer not in flight.
    pub(super) fn complete(&mut self, event: &Trb) -> Option<Done> {
        let position = self
            .in_flight
            .iter()
            .position(|&(p, _)| p == event.parameter)?;
        self.ring.retire(event.parameter).ok()?;
        let (_, tag) = self.in_flight.drain(..=position).next_back()?;
        Some(Done {
            tag,
            code: event.completion_code(),
            residual: event.residual(),
        })
    }

    /// Forget everything in flight after the endpoint halted or stopped;
    /// returns the pointer for Set TR Dequeue Pointer.
    pub(super) fn abandon(&mut self) -> u64 {
        self.in_flight.clear();
        self.ring.abandon()
    }
}

/// An interrupt-IN pipe that keeps [`REPORTS`] transfers queued.
pub(super) struct Reports {
    pub(super) pipe: Pipe,
    /// Offset of the report buffers in the device region.
    buffers: usize,
    len: u16,
    depth: usize,
    next: usize,
    /// Failed transfers in a row; reset by every good one.
    pub(super) errors: u32,
}

/// What a completed report transfer gave.
pub(super) enum Report {
    /// This many bytes, copied into the caller's buffer.
    Data(usize),
    /// The transfer failed with this completion code.
    Failed(u8),
    /// An event for nothing this pipe has in flight (stale): ignore it.
    Stale,
}

impl Reports {
    /// Reports from `pipe`, whose window starts at `window` in the device
    /// region; `depth` transfers are kept queued (1 for a hub's bitmap).
    pub(super) fn new(pipe: Pipe, window: usize, depth: usize) -> Reports {
        let len = pipe.context.max_packet.clamp(1, MAX_REPORT as u16);
        Reports {
            pipe,
            buffers: window + BUFFERS,
            len,
            depth: depth.clamp(1, REPORTS),
            next: 0,
            errors: 0,
        }
    }

    pub(super) fn dci(&self) -> u8 {
        self.pipe.dci
    }

    /// Top the ring up to the queue depth and ring once.
    ///
    /// Transfers complete in ring order and at most `depth` are in flight,
    /// so TRB `n` and TRB `n + depth` share a buffer only after TRB `n`
    /// completed and its report was read ([`Reports::take`]).
    pub(super) fn refill(&mut self, hc: &mut Hc, mem: &Region, slot: u8) -> Result<(), Error> {
        let mut added = false;
        while self.pipe.in_flight() < self.depth {
            let buffer = self.buffers + self.next * MAX_REPORT;
            self.pipe
                .submit(trb::interrupt_in(mem.bus(buffer), self.len), self.next)?;
            self.next = (self.next + 1) % self.depth;
            added = true;
        }
        if added {
            hc.doorbell(slot, self.pipe.dci);
        }
        Ok(())
    }

    /// Take a completed report into `out`.
    pub(super) fn take(&mut self, event: &Trb, mem: &Region, out: &mut [u8; MAX_REPORT]) -> Report {
        let Some(done) = self.pipe.complete(event) else {
            return Report::Stale;
        };
        if !matches!(done.code, code::SUCCESS | code::SHORT_PACKET) {
            self.errors += 1;
            return Report::Failed(done.code);
        }
        self.errors = 0;
        let residual = done.residual.min(u32::from(self.len)) as usize;
        let len = usize::from(self.len) - residual;
        mem.read(self.buffers + done.tag * MAX_REPORT, &mut out[..len]);
        Report::Data(len)
    }
}
