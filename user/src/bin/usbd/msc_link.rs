//! A stick's transport for `libs/usbmsc` (docs/architecture/usb-storage.md):
//! single-TRB bulk transfers through the controller's bulk window, control
//! requests on endpoint 0, and the controller-side recovery of a stalled or
//! failed bulk endpoint.
//!
//! Each transfer is one Normal TRB of at most 64 KiB from a window that
//! never crosses a 64 KiB boundary (`Hc::bulk`), waited for synchronously
//! with the controller's event timeout; events of other endpoints stay
//! queued for the dispatcher. A stalled endpoint is reset in the
//! controller once the library has cleared it on the device
//! (`reset_host_endpoint`); one that failed or timed out is stopped and its
//! ring skipped past the abandoned TRB (`Device::recover`). The Bulk-Only
//! recovery on the device side is the library's.

use usbmsc::bot::{Pipe as Transport, Setup, XferError};
use xhci::regs::portsc;
use xhci::trb::{self, code, kind, SetupPacket};

use super::device::Device;
use super::hc::{sleep_ms, Hc, BULK_WINDOW};
use super::pipe::Pipe;
use super::Error;

/// Stale events (of transfers abandoned earlier) skipped while waiting for
/// the current one before it counts as failed.
const STALE_EVENTS: usize = 8;

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
        let pipe = self.pipe(inbound);
        let dci = pipe.dci;
        pipe.submit(normal, 0).map_err(|_| XferError::Failed)?;
        self.hc.doorbell(slot, dci);
        for _ in 0..STALE_EVENTS {
            let waited = self.hc.wait(|e| {
                e.kind() == kind::TRANSFER_EVENT && e.slot() == slot && e.endpoint() == dci
            });
            let Ok(event) = waited else {
                break; // timed out
            };
            let Some(done) = self.pipe(inbound).complete(&event) else {
                continue; // an abandoned transfer's late event
            };
            return match done.code {
                code::SUCCESS | code::SHORT_PACKET => Ok(len - (done.residual as usize).min(len)),
                // Halted until the library clears it on the device and
                // calls `reset_host_endpoint`.
                code::STALL => Err(XferError::Stall),
                _ => Err(self.failed(inbound)),
            };
        }
        Err(self.failed(inbound))
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
