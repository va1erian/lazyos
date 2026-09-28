//! `clippaste` (`CLIPPS.ELF`): the clipboard paste demo (issue #115).
//!
//! `init` starts this program (and `clipcopy`) on every services boot. It:
//!
//! 1. subscribes to `session/<id>/clipboard/changed` (the offer is retained, so
//!    it is seen even when this task starts after `clipcopy`);
//! 2. pastes the offered `text/plain` payload by token and checks it is the
//!    lazily materialized value; prints `CLIP:PASTE:PASS`;
//! 3. stamps itself into a different session (a kernel-audited credential
//!    transition) and tries the same token again: `clipboardd` refuses a
//!    cross-session read with `-EACCES` and logs it, proving the deny path.
//!    It prints `CLIP:DENIED:PASS`.
//!
//! Any step that goes wrong prints `CLIP:<name>:FAIL:<detail>` and exits
//! non-zero.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;
use user::central;
use user::messenger::{self, clipboard, errno, Error};
use user::sys;

/// MIME type the demo copies and pastes.
const DEMO_MIME: &str = "text/plain";
/// PIT ticks `connect` retries while the supervisor's spawn settles.
const CONNECT_ATTEMPTS: usize = 64;
/// PIT ticks to wait for the changed event.
const CHANGED_TICKS: u64 = 1000;
/// The uid the denial probe switches to.
const PROBE_UID: u32 = 1000;
/// The session the denial probe switches to.
const PROBE_SESSION: u64 = 4242;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("clippaste: clipboard paste demo (issue #115)\n");
    if let Err(error) = run() {
        sys::write_str("clippaste: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1)
    }
    sys::exit(0)
}

/// Paste the demo offer, then prove the cross-session deny path.
fn run() -> messenger::Result<()> {
    let client = connect()?;
    // Watch the session's retained changed topic on the central broker first
    // (issue #169); a late subscriber is handed the offer.
    let mut changes_bus = central::Bus::connect_retry(64)?;
    let changes = changes_bus.subscribe(&clipboard::changes_topic(client.session()))?;
    let mut changed_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let event = match changes.recv_with(
        &mut changed_buffer,
        Some(sys::clock().saturating_add(CHANGED_TICKS)),
    )? {
        Some(event) => event,
        None => fail("PASTE", "no changed event"),
    };
    let info = clipboard::decode_changed(&event)?;
    if !info.mimes.iter().any(|mime| mime == DEMO_MIME) {
        fail("PASTE", "offer lacks the demo MIME type");
    }
    let bytes = client.paste_token(info.token, DEMO_MIME)?;
    if !bytes.starts_with(b"lazy@") || bytes.len() <= b"lazy@".len() {
        fail("PASTE", "payload is not the lazily materialized value");
    }
    sys::write_str(&format!(
        "CLIP:PASTE:PASS token={} bytes={}\n",
        info.token,
        bytes.len()
    ));

    // Cross-session policy probe: move to another session, then ask for the
    // same token. The service's `clipboard.read` scope must refuse it and log
    // the attempt (the credential transition itself is kernel-audited).
    let probe = sys::Cred::new(PROBE_UID, PROBE_UID, 0, 0, PROBE_SESSION);
    if sys::cred_set(None, &probe).is_err() {
        fail("DENIED", "could not enter the probe session");
    }
    match client.paste_token(info.token, DEMO_MIME) {
        Err(Error::Errno(code)) if code == -errno::EACCES => {
            sys::write_str("CLIP:DENIED:PASS\n");
        }
        Ok(_) => fail("DENIED", "cross-session paste was allowed"),
        Err(error) => fail("DENIED", error.message()),
    }
    // Release the broker-side subscription slot now that the paste and
    // denial probe are done: this demo client is about to exit, but a leaked
    // subscription would still fill the broker's bounded table on repeated
    // runs.
    let _ = changes.unsubscribe();
    Ok(())
}

/// Print `CLIP:<name>:FAIL:<detail>` and exit non-zero.
fn fail(name: &str, detail: &str) -> ! {
    sys::write_str(&format!("CLIP:{name}:FAIL:{detail}\n"));
    sys::exit(1)
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
