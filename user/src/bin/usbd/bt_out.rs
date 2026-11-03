//! Sending to the Bluetooth controller: the frames `btd` queued in the
//! `Commands` ring, one by one, in order.
//!
//! An HCI command goes out as a class request on endpoint 0 (`bmRequestType`
//! 0x20, `bRequest` 0, `wIndex` the interface): the request is synchronous,
//! like every control transfer `usbd` makes. ACL data goes out on the bulk-OUT
//! endpoint through the controller's shared bulk window, waited for like a
//! stick's transfer. A frame whose own length field does not match its size
//! is refused before it reaches the device (`libs/btusb`), so `btd` (or
//! whoever holds its uid) cannot make `usbd` send a malformed packet.

use user::sys;
use xhci::trb::{self, code, kind, request, SetupPacket};

use btusb::Outbound;
use user::messenger::usb_bt::wire;

use super::bt::BtFn;
use super::device::{Device, DATA_BYTES};
use super::hc::Hc;
use super::pipe::Pipe;

/// Frames taken from the ring per serve pass: the rest wait for the next, so
/// a flood cannot hold the controllers' event rings unserved.
const BATCH: usize = 16;
/// Ticks (100 Hz) an ACL transfer may take before it is given up. A bulk-OUT
/// packet the controller accepts completes in a millisecond or two; it is
/// waited for here in the main loop, so a radio that NAKs it (buffers full,
/// wedged) must not hold every other USB device for long: one wait, then the
/// pass ends (`pump_out`).
const ACL_TICKS: u64 = 5;
/// Ticks (100 Hz) an HCI command may take on endpoint 0. A controller takes
/// one in milliseconds; the wait holds every other USB device, so a wedged
/// dongle costs [`super::bt::MAX_EP0_FAILURES`] of these, then it is detached.
const COMMAND_TICKS: u64 = 100;
/// Late events of an abandoned transfer skipped while waiting for ours.
const STALE_EVENTS: usize = 4;

impl BtFn {
    /// Send what `btd` queued; whether there was anything. Then make sure a
    /// `Kick` will come for the next command (arm the ring and look once
    /// more), which is what lets `usbd` sleep.
    pub(super) fn serve(&mut self, hc: &mut Hc, device: &mut Device) -> bool {
        self.sink.finish();
        let mut busy = self.pump_out(hc, device);
        let mut gone = false;
        if let Some(link) = self.sink.link.as_mut() {
            gone = link.took_space() && !link.notify(1 << wire::NOTIFY_BIT_SPACE);
            if !gone && !busy && link.arm_commands() {
                busy = true;
            }
        }
        if gone {
            self.sink.drop_link("btd is gone");
        }
        busy
    }

    fn pump_out(&mut self, hc: &mut Hc, device: &mut Device) -> bool {
        let BtFn {
            interface,
            bulk_out,
            sink,
            scratch,
            ep0_failures,
            ..
        } = self;
        let Some(link) = sink.link.as_mut() else {
            return false;
        };
        let Some(pending) = link.pending() else {
            sink.poison();
            return false;
        };
        let mut served = 0;
        while served < BATCH.min(pending as usize) {
            served += 1;
            let len = match link.pop(scratch) {
                Ok(Some(len)) => len,
                Ok(None) => break,
                Err(()) => {
                    sink.poison();
                    return true;
                }
            };
            let stats = &mut sink.stats;
            match btusb::outbound(&scratch[..len]) {
                Ok(Outbound::Command(packet)) if packet.len() > DATA_BYTES => {
                    // Never reaches endpoint 0, so it says nothing about
                    // whether endpoint 0 answers: refused, not an EP0 failure
                    // (which would give the device up).
                    stats.refused += 1;
                }
                Ok(Outbound::Command(packet)) => {
                    let setup = SetupPacket {
                        request_type: 0x20,
                        request: 0,
                        value: 0,
                        index: u16::from(*interface),
                        length: packet.len() as u16,
                    };
                    if device
                        .control_out_data_within(hc, setup, packet, Some(COMMAND_TICKS))
                        .is_ok()
                    {
                        stats.commands += 1;
                        *ep0_failures = 0;
                    } else {
                        // Endpoint 0 is not answering: end the pass rather
                        // than wait on each queued command in turn, and give
                        // the device up if it keeps happening.
                        stats.errors += 1;
                        *ep0_failures += 1;
                        break;
                    }
                }
                Ok(Outbound::Acl(packet)) => {
                    if send_acl(hc, device, bulk_out, packet) {
                        stats.acl_out += 1;
                    } else {
                        // The radio is not taking data: leave the rest for a
                        // later pass rather than wait on each in turn.
                        stats.errors += 1;
                        break;
                    }
                }
                Err(_) => stats.refused += 1,
            }
        }
        served > 0
    }
}

/// One ACL packet on the bulk-OUT endpoint; whether the device took it.
fn send_acl(hc: &mut Hc, device: &mut Device, pipe: &mut Pipe, packet: &[u8]) -> bool {
    let bus = match hc.bulk() {
        Ok((region, offset)) => {
            region.write(offset, packet);
            region.bus(offset)
        }
        Err(_) => return false,
    };
    let Some(normal) = trb::bulk(bus, packet.len() as u32) else {
        return false;
    };
    if pipe.submit(normal, 0).is_err() {
        return false;
    }
    let (slot, dci) = (device.slot, pipe.dci);
    hc.doorbell(slot, dci);
    let deadline = sys::clock() + ACL_TICKS;
    for _ in 0..STALE_EVENTS {
        let waited = hc.wait_until(deadline, |e| {
            e.kind() == kind::TRANSFER_EVENT && e.slot() == slot && e.endpoint() == dci
        });
        let Ok(event) = waited else {
            break;
        };
        // An abandoned transfer's late event is not ours: wait on.
        let Some(done) = pipe.complete(&event) else {
            continue;
        };
        if matches!(done.code, code::SUCCESS | code::SHORT_PACKET) {
            return true;
        }
        recover(hc, device, pipe, done.code);
        return false;
    }
    recover(hc, device, pipe, 0);
    false
}

/// Bring the OUT endpoint back after a failure or a timeout: reset or stop
/// it, skip what was queued, clear a device-side halt after a stall.
fn recover(hc: &mut Hc, device: &mut Device, pipe: &mut Pipe, failed: u8) {
    let dci = pipe.dci;
    let pointer = pipe.abandon();
    if device.recover(hc, dci, pointer).is_err() {
        return;
    }
    if failed == code::STALL {
        let address = (dci / 2) | (dci & 1) << 7;
        let _ = device.control_out(hc, request::clear_endpoint_halt(address));
    }
}
