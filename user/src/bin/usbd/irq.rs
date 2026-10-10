//! Interrupt-driven idling and event waits (docs/performance-plan.md P3.7,
//! usb-hid-plan risk 10, issue #719).
//!
//! `usbd` used to nap one PIT tick between looks at its event rings, which
//! put up to 10 ms between a key or mouse report reaching the controller and
//! `inputd` hearing of it, and a whole tick between every step of a stick
//! request (command, data, status: tens of ms per block). Each controller's
//! interrupter 0 now raises the claim's INTx line when an event lands; the
//! kernel posts that as a message on the channel it made at claim time (the
//! mechanism `sndd` and `netdrv` use). The idle loop parks on every
//! controller's endpoint at once with the Messenger wait set, and an event
//! wait ([`wait_event`], a transfer or a command in flight) parks on its own
//! controller's, so a bulk transfer completes at the device's latency rather
//! than the next tick. A slow poll ([`FALLBACK_TICKS`], and one tick for an
//! event wait) stays as a safety net: a lost or unroutable interrupt costs
//! at most that, never a hang, and a claim whose interrupts never arrive
//! runs exactly as the polled driver did.
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
/// Longest one event wait ([`wait_event`]) parks without an interrupt: one
/// tick, exactly the poll it replaces, so a lost or unroutable interrupt
/// costs one tick, never a hang, and a claim whose interrupts never arrive
/// is no slower than the polled driver.
const EVENT_FALLBACK_TICKS: u64 = 1;
/// Bytes of one interrupt message's receive buffer (the kernel's is three
/// TLV `u32`s; the main loop's `irq_buf` is sized by this).
pub(super) const MESSAGE_BYTES: usize = 256;
/// Interrupt moderation, 250 ns units: 40 us, far below a HID poll interval,
/// so a burst of events costs one interrupt rather than one each.
const IMOD: u32 = 160;
/// PCI command register and its INTx-disable bit.
const PCI_COMMAND: u64 = 4;
const PCI_INTX_DISABLE: u64 = 1 << 10;

/// Claim `row` with an interrupt channel (the kernel makes it, issue #496),
/// or without one (polling) if the kernel refuses it.
pub(super) fn claim(row: &Row) -> Result<(u64, Option<Endpoint>), Error> {
    if let Ok((handle, channel)) = dev::claim_with_irq(row.id, false) {
        return Ok((handle, Some(Endpoint::from_raw(channel))));
    }
    Ok((dev::claim(row.id).map_err(Error::Dev)?, None))
}

/// Arm the claim's line once the controller runs: the kernel keeps INTx off
/// until then, so the device is let assert it and interrupter 0 enabled.
/// Any refusal leaves the controller polled, which is always correct.
pub(super) fn arm(hc: &mut Hc, endpoint: Option<Endpoint>) {
    let Some(endpoint) = endpoint else {
        return;
    };
    let Ok(mode) = dev::irq_enable(hc.handle) else {
        sys::write_str(&format!("USBD:IRQ hc={} polled (no line)\n", hc.index));
        return;
    };
    // On MSI or MSI-X the kernel programmed the message; INTx needs the
    // function let assert its line. Interrupter 0 signals either way.
    if mode == dev::IrqMode::Intx {
        if let Ok(command) = dev::cfg_read(hc.handle, PCI_COMMAND, 2) {
            let command = u64::from(command) & !PCI_INTX_DISABLE;
            let _ = dev::cfg_write(hc.handle, PCI_COMMAND, 2, command);
        }
    }
    hc.enable_interrupter(IMOD);
    hc.irq = Some(endpoint);
    sys::write_str(&format!("USBD:IRQ hc={} armed {mode:?}\n", hc.index));
}

/// Take and acknowledge every interrupt message already queued, without
/// waiting. Only the kernel (sender 0) may send one, naming a device.
pub(super) fn service(controllers: &mut [Controller], buf: &mut [u8]) {
    for controller in controllers {
        acknowledge(&mut controller.hc, buf);
    }
}

/// One controller's half of [`service`]: every queued message taken,
/// validated and acknowledged, the interrupter's pending flag cleared
/// before the acknowledgement that unmasks the line.
fn acknowledge(hc: &mut Hc, buf: &mut [u8]) {
    let Some(endpoint) = hc.irq else {
        return;
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

/// Wait for the next event before another look at the ring (issue #719):
/// parked on the claim's interrupt endpoint until the kernel posts the next
/// interrupt, so a command or a bulk transfer in flight (a stick request is
/// command, data and status) completes at the device's latency instead of
/// the next tick. The park is bounded by [`EVENT_FALLBACK_TICKS`] (and the
/// caller's deadline), so a lost or unroutable interrupt costs one poll,
/// never a hang; without an armed interrupt this is the old one-tick nap.
/// Woken, the posted messages are taken and acknowledged exactly as
/// [`service`] does, so the line is live for the event after the one the
/// caller is about to drain.
pub(super) fn wait_event(hc: &mut Hc, deadline: u64) {
    let Some(endpoint) = hc.irq else {
        return super::hc::nap();
    };
    let now = sys::clock();
    let bounded = now.saturating_add(EVENT_FALLBACK_TICKS).min(deadline);
    if bounded <= now {
        // Already at a bound: sleep the tick the caller's timeout check
        // needs, never a parked spin.
        return super::hc::nap();
    }
    match wait::wait_any(&[endpoint], 0, Some(bounded)) {
        Ok(_) => {
            let mut buf = [0u8; MESSAGE_BYTES];
            acknowledge(hc, &mut buf);
        }
        Err(MsgError::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {}
        // Never spin on a refused wait.
        Err(_) => super::hc::nap(),
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
