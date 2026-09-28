//! `clipcopy` (`CLIPCP.ELF`): the lazy clipboard owner demo (issue #115).
//!
//! `init` starts this program (and `clippaste`) on every services boot, so a
//! headless run proves the clipboard path with machine-parseable serial
//! markers:
//!
//! 1. register a sink endpoint, publish a **lazy** offer for `text/plain`, and
//!    print `CLIP:COPY:PASS token=<n>`;
//! 2. wait for one `Serialize` callback from `clipboardd`, materialize the
//!    payload *then* (the payload embeds the tick, so it cannot have existed at
//!    offer time), answer, and print `CLIP:LAZY:PASS`.
//!
//! A missed callback prints `CLIP:COPY:FAIL:<detail>` and exits non-zero, so a
//! regression is loud.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::central;
use user::messenger::{self, clipboard, errno, registry, Endpoint, Error};
use user::sys;

/// Registry name this demo serves `Serialize` under.
const SINK: &str = "os.lazy.clipboard.owner.clipcopy";
/// The one MIME type the demo offers.
const DEMO_MIME: &str = "text/plain";
/// PIT ticks `connect` retries while the supervisor's spawn settles.
const CONNECT_ATTEMPTS: usize = 64;
/// PIT ticks to wait for a paste's serialize request.
const SERVE_TICKS: u64 = 1000;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("clipcopy: lazy clipboard owner (issue #115)\n");
    if let Err(error) = run() {
        sys::write_str("clipcopy: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Offer lazily, then serve exactly one `Serialize` request.
fn run() -> messenger::Result<()> {
    let client = connect()?;
    // Register the owner endpoint before the offer, so the service can resolve
    // it the moment a paste happens.
    let (published, server) = messenger::create_pair()?;
    registry::register(SINK, &published, &[clipboard::OWNER_INTERFACE], 0)?;
    // The changed topic lives on `messengerd`'s central broker (issue #169);
    // the broker replays the retained offer to a late subscriber.
    let mut changes_bus = central::Bus::connect_retry(64)?;
    let changes = changes_bus.subscribe(&clipboard::changes_topic(client.session()))?;
    let token = client.offer_lazy("clipcopy", SINK, &[DEMO_MIME])?;
    sys::write_str(&format!("CLIP:COPY:PASS token={token}\n"));
    // Drain the changed event: the owner watches its own session too.
    let mut changed_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let _ = changes.recv_with(&mut changed_buffer, Some(sys::clock().saturating_add(50)));
    serve_serialize(&server)
}

/// Serve one `Serialize` request, then stop: the demo needs exactly one paste.
fn serve_serialize(server: &Endpoint) -> messenger::Result<()> {
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let deadline = sys::clock().saturating_add(SERVE_TICKS);
    loop {
        match server.recv_with(&mut buffer, Some(deadline)) {
            Ok(message) => {
                if message.interface_id() != clipboard::OWNER_INTERFACE
                    || message.method() != clipboard::method::SERIALIZE
                {
                    continue;
                }
                let (token, mime) = clipboard::decode_serialize(&message.parcel)?;
                let bytes = payload(&mime);
                if let Some(txn) = message.txn {
                    server.reply(txn, &clipboard::serialize_reply(&bytes)?)?;
                }
                sys::write_str(&format!(
                    "CLIP:LAZY:PASS token={token} mime={mime} bytes={}\n",
                    bytes.len()
                ));
                return Ok(());
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {
                sys::write_str("CLIP:COPY:FAIL:no paste asked for the offer\n");
                sys::exit(1);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Materialize the demo payload now (the point of lazy transfer).
fn payload(mime: &str) -> Vec<u8> {
    format!("lazy@{}:{mime}", sys::clock()).into_bytes()
}

/// Resolve `clipboardd`, retrying while its registration lands.
fn connect() -> messenger::Result<clipboard::Client> {
    let mut last = Error::Errno(-errno::ENOENT);
    for _ in 0..CONNECT_ATTEMPTS {
        match clipboard::Client::connect() {
            Ok(client) => return Ok(client),
            Err(error) => last = error,
        }
        park_tick();
    }
    Err(last)
}

/// Sleep one PIT tick by parking on a private channel pair with an expired
/// deadline (userspace has no sleep syscall; the topics client uses the same
/// trick). The pair is closed again so no channel leaks.
fn park_tick() {
    if let Ok((probe, peer)) = messenger::create_pair() {
        let mut scratch = [0u8; 16];
        let _ = probe.recv_into(&mut scratch, Some(messenger::EXPIRED_DEADLINE));
        let _ = probe.close();
        let _ = peer.close();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
