//! The driver's own proof that frames cross the real device in both
//! directions, before any Messenger client exists: it attaches itself to the
//! engine as a client, broadcasts an ARP request for the gateway and waits for
//! the reply. The request and the reply are in the packet capture
//! (`tools/net/run.py`), which is the actual verdict; the serial marker only
//! says the driver is done.
//!
//! Going through the engine and its rings, rather than poking the queues,
//! means the self-test exercises the same path a real client uses.

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::format;

use framering::{ring_bytes, Ring, MAX_FRAME};
use nicdrv::arp;
use user::messenger::Endpoint;
use user::sys;

use super::card::Card;
use super::error::Error;

/// The address QEMU's user-mode network gives the guest, and its gateway.
const OUR_IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
const SLOTS: u32 = 16;
/// Ticks to wait for the reply, and between repeats of the request (the first
/// can be lost while the host side is still coming up).
const TIMEOUT_TICKS: u64 = 400;
const RESEND_TICKS: u64 = 50;
/// The owner recorded for the driver's own attachment.
const SELF_OWNER: u64 = u64::MAX;

pub(super) fn run(card: &mut Card, server: &Endpoint) -> Result<(), Error> {
    let one = ring_bytes(SLOTS);
    let len = one * 2;
    let layout = Layout::from_size_align(len, 4096).map_err(|_| Error::SelfTest("layout"))?;
    // SAFETY: the layout has a non-zero size; the memory is zeroed, page
    // aligned and never freed (one allocation for the life of the driver), so
    // the rings stay valid for as long as the engine may use them.
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        return Err(Error::SelfTest("out of memory"));
    }
    // SAFETY: both rings lie inside the allocation above.
    let (rx, tx) = unsafe {
        let rx = Ring::create(base, one, SLOTS);
        let tx = Ring::create(base.add(one), one, SLOTS);
        (rx, tx)
    };
    let (Ok(rx), Ok(tx)) = (rx, tx) else {
        return Err(Error::SelfTest("ring layout"));
    };
    let (mut rx, mut tx) = (rx.consumer(), tx.producer());
    // SAFETY: `base` is valid for `len` bytes for the rest of the process.
    let ring = unsafe { card.engine.attach(SELF_OWNER, SLOTS, base, len) }
        .map_err(|_| Error::SelfTest("attach refused"))?;

    let mac = card.engine.mac();
    let request = arp::request(mac, OUR_IP, GATEWAY_IP);
    let deadline = sys::clock() + TIMEOUT_TICKS;
    let mut next_send = 0;
    let mut answered = None;
    let mut frame = [0u8; MAX_FRAME];
    while answered.is_none() && sys::clock() < deadline {
        if sys::clock() >= next_send {
            // A full ring just means an earlier request is still queued.
            let _ = tx.push(&request);
            next_send = sys::clock() + RESEND_TICKS;
        }
        card.pump()?;
        while let Ok(Some(n)) = rx.pop(&mut frame) {
            if let Some(mac) = arp::reply_from(&frame[..n], mac, GATEWAY_IP) {
                answered = Some(mac);
            }
        }
        if answered.is_none() {
            super::wait_event(card, server);
        }
    }
    let _ = card.engine.detach(SELF_OWNER, ring);
    match answered {
        Some(from) => {
            sys::write_str(&format!(
                "NET:NIC:PASS mac={} link={} mtu={} rx={} tx={} arp_reply_from={}\n",
                super::mac_text(&mac),
                card.engine.link(),
                card.mtu,
                card.queue_sizes.0,
                card.queue_sizes.1,
                super::mac_text(&from)
            ));
            Ok(())
        }
        None => Err(Error::SelfTest("no ARP reply from the gateway")),
    }
}
