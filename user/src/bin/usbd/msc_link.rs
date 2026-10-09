//! A stick's transport for `libs/usbmsc` (docs/architecture/usb-storage.md):
//! single-TRB bulk transfers through the controller's bulk window, control
//! requests on endpoint 0, and the controller-side recovery of a stalled or
//! failed bulk endpoint.
//!
//! Each transfer is one Normal TRB of at most 64 KiB from a window that
//! never crosses a 64 KiB boundary (`Hc::bulk`), waited for synchronously
//! up to the link's [`Patience`]; events of other endpoints stay queued for
//! the dispatcher. Serving a request, a transfer may take
//! [`SERVE_TRANSFER_TICKS`] (a real stick stalls a write for seconds while
//! its flash reorganises; Linux allows a SCSI command 30 s), and the whole
//! request, retries and recovery included, ends by its budget so the
//! kernel's 60 s deadline never fires first (issue #704). A failed transfer
//! prints `USBD:MSC:XFER` with what the controller made of it. A stalled
//! endpoint is reset in the
//! controller once the library has cleared it on the device
//! (`reset_host_endpoint`); one that failed or timed out is stopped and its
//! ring skipped past the abandoned TRB (`Device::recover`). The Bulk-Only
//! recovery on the device side is the library's.

use alloc::format;
use alloc::string::String;

use usbmsc::bot::{Pipe as Transport, Setup, XferError};
use user::sys;
use xhci::regs::portsc;
use xhci::trb::{self, code, kind, SetupPacket};

use super::device::Device;
use super::hc::{sleep_ms, Hc, BULK_WINDOW};
use super::pipe::Pipe;
use super::Error;

/// Stale events (of transfers abandoned earlier) skipped while waiting for
/// the current one before it counts as failed.
const STALE_EVENTS: usize = 8;
/// How long one bulk transfer may take while a request is served (PIT
/// ticks, 100 Hz): 30 s, Linux's SCSI command timeout.
pub(super) const SERVE_TRANSFER_TICKS: u64 = 3000;
/// The most one kernel request may take, retries included: 45 s, under the
/// kernel's 60 s (`block::provider::TAKEN_TICKS`).
pub(super) const REQUEST_BUDGET_TICKS: u64 = 4500;
/// How long one bulk transfer may take during bring-up (5 s): a stick that
/// does not answer INQUIRY should not hold the other devices up.
pub(super) const BRING_UP_TRANSFER_TICKS: u64 = 500;

/// How long the link waits: per transfer, and for everything until `until`
/// (absolute ticks; a transfer past it fails at once).
#[derive(Clone, Copy, Debug)]
pub(super) struct Patience {
    pub(super) transfer: u64,
    pub(super) until: u64,
}

impl Patience {
    /// Serving one kernel request, starting now.
    pub(super) fn request() -> Patience {
        Patience {
            transfer: SERVE_TRANSFER_TICKS,
            until: sys::clock() + REQUEST_BUDGET_TICKS,
        }
    }

    /// Bringing a stick up: short transfers, no overall bound (the bring-up
    /// sequence bounds its own loops).
    pub(super) fn bring_up() -> Patience {
        Patience {
            transfer: BRING_UP_TRANSFER_TICKS,
            until: u64::MAX,
        }
    }

    /// The deadline of a transfer starting now.
    fn deadline(&self) -> u64 {
        sys::clock().saturating_add(self.transfer).min(self.until)
    }
}

/// A Bulk-Only interface's two pipes and the addresses the library names.
pub(super) struct Pipes {
    pub(super) interface: u8,
    pub(super) bulk_in: Pipe,
    pub(super) bulk_out: Pipe,
    pub(super) in_address: u8,
    pub(super) out_address: u8,
}

/// The controller, the device and its pipes: the transport `usbmsc` drives.
pub(super) struct Link<'a> {
    pub(super) hc: &'a mut Hc,
    pub(super) device: &'a mut Device,
    pub(super) pipes: &'a mut Pipes,
    pub(super) patience: Patience,
}

impl Link<'_> {
    fn pipe(&mut self, inbound: bool) -> &mut Pipe {
        if inbound {
            &mut self.pipes.bulk_in
        } else {
            &mut self.pipes.bulk_out
        }
    }

    /// One bulk transfer of `len` bytes through the window; the bytes moved.
    fn transfer(&mut self, inbound: bool, len: usize) -> Result<usize, XferError> {
        let bus = match self.hc.bulk() {
            Ok((region, offset)) => region.bus(offset),
            Err(_) => return Err(XferError::Failed),
        };
        let normal = trb::bulk(bus, len as u32).ok_or(XferError::Failed)?;
        let slot = self.device.slot;
        let dci = self.pipe(inbound).dci;
        let started = sys::clock();
        let deadline = self.patience.deadline();
        if started >= deadline {
            // The request's budget is spent: fail without touching the bus.
            self.report(inbound, len, "budget", started);
            return Err(XferError::Failed);
        }
        self.pipe(inbound)
            .submit(normal, 0)
            .map_err(|_| XferError::Failed)?;
        self.hc.doorbell(slot, dci);
        let mut outcome = String::from("timeout");
        for _ in 0..STALE_EVENTS {
            let waited = self.hc.wait_until(deadline, |e| {
                e.kind() == kind::TRANSFER_EVENT && e.slot() == slot && e.endpoint() == dci
            });
            let Ok(event) = waited else {
                break; // timed out
            };
            let Some(done) = self.pipe(inbound).complete(&event) else {
                continue; // an abandoned transfer's late event
            };
            match done.code {
                code::SUCCESS | code::SHORT_PACKET => {
                    return Ok(len - (done.residual as usize).min(len))
                }
                // Halted until the library clears it on the device and
                // calls `reset_host_endpoint`.
                code::STALL => {
                    self.report(inbound, len, "stall", started);
                    return Err(XferError::Stall);
                }
                other => {
                    outcome = format!("{other}");
                    break;
                }
            }
        }
        self.report(inbound, len, &outcome, started);
        Err(self.failed(inbound))
    }

    /// One `USBD:MSC:XFER` line for a transfer that did not complete:
    /// direction and length, the outcome (`timeout`, `stall`, `budget` when
    /// the request's time was spent, or the completion code), how long it
    /// was waited for, and the endpoint's state and dequeue pointer as the
    /// controller recorded them (before recovery).
    fn report(&mut self, inbound: bool, len: usize, outcome: &str, started: u64) {
        let dci = self.pipe(inbound).dci;
        let (state, dequeue) = self.device.endpoint_state(self.hc, dci);
        sys::write_str(&format!(
            "USBD:MSC:XFER port={} {} len={len} result={outcome} waited_ms={} epstate={state} epdq={dequeue:#x} {}\n",
            self.device.name,
            if inbound { "in" } else { "out" },
            sys::clock().saturating_sub(started) * 10,
            self.hc.state_text()
        ));
    }

    /// A transfer that failed or timed out: `Gone` when the device left,
    /// else the endpoint is recovered and the failure reported.
    fn failed(&mut self, inbound: bool) -> XferError {
        if !self.present() {
            return XferError::Gone;
        }
        match self.recover(inbound) {
            Ok(()) => XferError::Failed,
            Err(error) => error,
        }
    }

    /// Reset (halted) or stop (running) the endpoint and point it past
    /// everything abandoned; its stale events are dropped.
    fn recover(&mut self, inbound: bool) -> Result<(), XferError> {
        let pipe = self.pipe(inbound);
        let dci = pipe.dci;
        let pointer = pipe.abandon();
        match self.device.recover(self.hc, dci, pointer) {
            Ok(()) => Ok(()),
            Err(_) if !self.present() => Err(XferError::Gone),
            Err(_) => Err(XferError::Failed),
        }
    }

    /// Whether the device is still plugged in. Only a root port can be
    /// asked cheaply; below a hub the hub's own report says, later.
    fn present(&self) -> bool {
        let at = &self.device.at;
        at.depth != 0 || self.hc.portsc(at.root_port) & portsc::CCS != 0
    }
}

impl Transport for Link<'_> {
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, XferError> {
        if data.len() > BULK_WINDOW {
            return Err(XferError::Failed);
        }
        match self.hc.bulk() {
            Ok((region, offset)) => region.write(offset, data),
            Err(_) => return Err(XferError::Failed),
        }
        self.transfer(false, data.len())
    }

    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<usize, XferError> {
        if buf.len() > BULK_WINDOW {
            return Err(XferError::Failed);
        }
        let got = self.transfer(true, buf.len())?;
        match self.hc.bulk() {
            Ok((region, offset)) => region.read(offset, &mut buf[..got]),
            Err(_) => return Err(XferError::Failed),
        }
        Ok(got)
    }

    fn control(&mut self, setup: Setup) -> Result<(), XferError> {
        let packet = SetupPacket {
            request_type: setup.request_type,
            request: setup.request,
            value: setup.value,
            index: setup.index,
            length: 0,
        };
        // `Device::control` recovers endpoint 0 itself after a failure.
        match self.device.control_out(self.hc, packet) {
            Ok(()) => Ok(()),
            Err(Error::Completion(_, code::STALL)) => Err(XferError::Stall),
            Err(_) if !self.present() => Err(XferError::Gone),
            Err(_) => Err(XferError::Failed),
        }
    }

    fn reset_host_endpoint(&mut self, inbound: bool) -> Result<(), XferError> {
        self.recover(inbound)
    }

    fn endpoints(&self) -> (u8, u8, u8) {
        (
            self.pipes.in_address,
            self.pipes.out_address,
            self.pipes.interface,
        )
    }

    fn delay_ms(&mut self, ms: u32) {
        sleep_ms(u64::from(ms));
    }
}
