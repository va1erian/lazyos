//! Interrupt-driven idling (docs/performance-plan.md P3.7, usb-hid-plan
//! risk 10).
//!
//! `usbd` used to nap one PIT tick between looks at its event rings, which
//! put up to 10 ms between a key or mouse report reaching the controller and
//! `inputd` hearing of it. Each controller's interrupter 0 now raises the
//! claim's INTx line when an event lands; the kernel posts that as a message
//! to the endpoint named at claim time (the mechanism `sndd` and `netdrv`
//! use), and the idle loop parks on every controller's endpoint at once with
//! the Messenger wait set. A slow poll ([`FALLBACK_TICKS`]) stays as a safety
//! net: a lost or unroutable interrupt costs at most that, never a hang.
//!
//! Order inside one wake: clear the interrupter's pending flag, acknowledge
//! the message (the kernel unmasks the line), then drain the ring. An event
//! that lands after the clear sets the flag again and raises a new interrupt,
//! so none is missed between the drain and the next park.

use alloc::format;
use alloc::vec::Vec;

use user::dev::{self, Row};
use user::messenger::{self, wait, Endpoint, Error as MsgError};
use user::sys;

use super::bus::Controller;
use super::hc::Hc;
use super::Error;

/// Longest an idle driver sleeps without an interrupt (PIT ticks).
const FALLBACK_TICKS: u64 = 10;
/// Interrupt moderation, 250 ns units: 40 us, far below a HID poll interval,
/// so a burst of events costs one interrupt rather than one each.
const IMOD: u32 = 160;
/// PCI command register and its INTx-disable bit.
const PCI_COMMAND: u64 = 4;
const PCI_INTX_DISABLE: u64 = 1 << 10;

/// Claim `row` with an interrupt endpoint, or without one (polling) if the
/// kernel refuses it. The channel's other side is dropped deliberately
/// unused but open, so the endpoint never reports a dead peer.
pub(super) fn claim(row: &Row) -> Result<(u64, Option<Endpoint>), Error> {
    if let Ok((side, _peer)) = messenger::create_pair() {
        if let Ok(handle) = dev::claim(row.id, Some(side.handle()), false) {
            return Ok((handle, Some(side)));
        }
    }
    Ok((dev::claim(row.id, None, false).map_err(Error::Dev)?, None))
}

/// Arm the claim's line once the controller runs: the kernel keeps INTx off
/// until then, so the device is let assert it and interrupter 0 enabled.
/// Any refusal leaves the controller polled, which is always correct.
pub(super) fn arm(hc: &mut Hc, endpoint: Option<Endpoint>) {
    let Some(endpoint) = endpoint else {
        return;
    };
    if dev::irq_enable(hc.handle).is_err() {
        sys::write_str(&format!("USBD:IRQ hc={} polled (no line)\n", hc.index));
        return;
    }
    if let Ok(command) = dev::cfg_read(hc.handle, PCI_COMMAND, 2) {
        let command = u64::from(command) & !PCI_INTX_DISABLE;
        let _ = dev::cfg_write(hc.handle, PCI_COMMAND, 2, command);
    }
    hc.enable_interrupter(IMOD);
    hc.irq = Some(endpoint);
    sys::write_str(&format!("USBD:IRQ hc={} armed\n", hc.index));
}

/// Take and acknowledge every interrupt message already queued, without
/// waiting. Only the kernel (sender 0) may send one, naming a device.
pub(super) fn service(controllers: &mut [Controller], buf: &mut [u8]) {
    for controller in controllers {
        let hc = &mut controller.hc;
        let Some(endpoint) = hc.irq else {
            continue;
        };
        while let Ok(Some(message)) = endpoint.poll_recv_with(buf) {
            if message.sender != 0 || dev::parse_irq_body(&message.parcel.body).is_none() {
                sys::write_str(&format!(
                    "USBD:IRQ:REJECT hc={} sender={}\n",
                    hc.index, message.sender
                ));
                continue;
            }
            hc.clear_interrupt();
            let _ = dev::irq_ack(hc.handle);
        }
    }
}

/// Idle: park until a controller interrupts or [`FALLBACK_TICKS`] pass.
/// Without any armed controller this is the old one-tick nap.
pub(super) fn park(controllers: &[Controller]) {
    let endpoints: Vec<Endpoint> = controllers
        .iter()
        .filter_map(|controller| controller.hc.irq)
        .take(wait::MAX_ENDPOINTS)
        .collect();
    if endpoints.is_empty() {
        return super::hc::nap();
    }
    match wait::wait_any(&endpoints, 0, Some(sys::clock() + FALLBACK_TICKS)) {
        Ok(_) => {}
        Err(MsgError::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {}
        // Never spin on a refused wait.
        Err(_) => super::hc::nap(),
    }
}
