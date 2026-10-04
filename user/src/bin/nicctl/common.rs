//! Helpers shared by `nicctl`'s modes: connecting to the driver, sleeping, the
//! one ARP exchange every mode that needs real traffic uses, and turning
//! Messenger failures into log text.

use alloc::format;
use alloc::string::String;

use nicdrv::arp;
use user::messenger::net::{self as api, Attachment};
use user::messenger::Error as MsgError;
use user::sys;

/// Ticks (100 Hz) to wait for the driver to register before giving up.
const CONNECT_TICKS: u64 = 500;
/// The address QEMU's user-mode network gives the guest, and its gateway.
pub(super) const OUR_IP: [u8; 4] = [10, 0, 2, 15];
pub(super) const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
/// Ticks to wait for an ARP reply, and between repeats of the request.
const ARP_TIMEOUT_TICKS: u64 = 400;
const ARP_RESEND_TICKS: u64 = 50;

/// Sleep one PIT tick ([`sys::nap`]).
pub(super) fn nap() {
    sys::nap();
}

/// Prefix a Messenger failure with what was being attempted.
pub(super) fn fail(what: &str) -> impl Fn(MsgError) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

/// `52:54:00:12:34:56`.
pub(super) fn mac_text(mac: &[u8]) -> String {
    let mut text = String::new();
    for (i, byte) in mac.iter().enumerate() {
        if i > 0 {
            text.push(':');
        }
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// Resolve the driver, retrying while it is still starting up.
pub(super) fn connect() -> Result<api::Client, String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    loop {
        match api::Client::connect() {
            Ok(client) => return Ok(client),
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no NIC service: {}", error.message()))
            }
            Err(_) => nap(),
        }
    }
}

/// Whether `result` failed with exactly `errno` (a positive errno value).
pub(super) fn is_errno<T>(result: &Result<T, MsgError>, errno: i64) -> bool {
    matches!(result, Err(MsgError::Errno(code)) if *code == -errno)
}

/// Broadcast an ARP request for `target` and wait for its reply through the
/// attached rings: the same exchange a real stack makes, visible in the packet
/// capture. Returns the MAC that answered.
pub(super) fn arp_exchange(
    client: &api::Client,
    attachment: &mut Attachment,
    mac: [u8; 6],
    target: [u8; 4],
) -> Result<[u8; 6], String> {
    let request = arp::request(mac, OUR_IP, target);
    let deadline = sys::clock() + ARP_TIMEOUT_TICKS;
    let mut next_send = 0;
    let mut frame = [0u8; framering::MAX_FRAME];
    loop {
        let now = sys::clock();
        if now >= next_send {
            // A full ring just means an earlier request is still queued.
            if attachment.tx.push(&request).is_ok() && attachment.tx.take_notify() {
                client.kick(attachment.ring).map_err(fail("kick"))?;
            }
            next_send = now + ARP_RESEND_TICKS;
        }
        while let Ok(Some(n)) = attachment.rx.pop(&mut frame) {
            if let Some(from) = arp::reply_from(&frame[..n], mac, target) {
                return Ok(from);
            }
        }
        if now >= deadline {
            return Err(String::from("no ARP reply"));
        }
        // Ask for a wake-up, look once more so a frame that landed in between
        // is not missed, then sleep until a notice or the next resend.
        attachment.rx.arm();
        if !matches!(attachment.rx.pending(), Ok(0)) {
            continue;
        }
        attachment
            .wait(next_send.min(deadline))
            .map_err(fail("waiting for a notice"))?;
    }
}
