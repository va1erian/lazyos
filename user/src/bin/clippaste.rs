//! `clippaste` (`/system/bin/clippaste`): the clipboard paste demo (issue #115).
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
use user::trace;

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
    let session_text = format!("{}", client.session());
    let changes = clipboard::subscribe_session_clipboard_changed(&mut changes_bus, &session_text)?;
    let mut changed_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let event = match changes.recv_with(
        &mut changed_buffer,
        Some(sys::clock().saturating_add(CHANGED_TICKS)),
    )? {
        Some(event) => event,
        None => fail("PASTE", "no changed event"),
    };
    let meta =
        clipboard::decode_session_clipboard_changed(&event.payload).map_err(Error::Parcel)?;
    let info = clipboard::from_wire_meta(meta);
    if !info.mimes.iter().any(|mime| mime == DEMO_MIME) {
        fail("PASTE", "offer lacks the demo MIME type");
    }
    let bytes = client.paste_token(info.token, DEMO_MIME)?;
    if !bytes.starts_with(b"lazy@") || bytes.len() <= b"lazy@".len() {
        fail("PASTE", "payload is not the lazily materialized value");
    }
    trace::line!(
        trace::Channel::Clip,
        "PASTE:PASS token={} bytes={}",
        info.token,
        bytes.len()
    );

    // Cross-session policy probe: move to another session, then ask for the
    // same token. The service's `clipboard.read` scope must refuse it and log
    // the attempt (the credential transition itself is kernel-audited).
    let probe = sys::Cred::new(PROBE_UID, PROBE_UID, 0, 0, PROBE_SESSION);
    if sys::cred_set(None, &probe).is_err() {
        fail("DENIED", "could not enter the probe session");
    }
    match client.paste_token(info.token, DEMO_MIME) {
        Err(Error::Errno(code)) if code == -errno::EACCES => {
            trace::line!(trace::Channel::Clip, "DENIED:PASS");
        }
        Ok(_) => fail("DENIED", "cross-session paste was allowed"),
        Err(error) => fail("DENIED", error.message()),
    }
    // The large-offer probes run last: the session keeps one offer, so they
    // would evict the one the denial probe above names.
    big_offer_probes(&client)?;
    // Release the broker-side subscription slot now that the paste and
    // denial probe are done: this demo client is about to exit, but a leaked
    // subscription would still fill the broker's bounded table on repeated
    // runs.
    let _ = changes.unsubscribe();
    Ok(())
}

/// Bytes of the large offer the probes make (past the inline cap).
const BIG_LEN: usize = 20_000;
/// MIME type of the large probe offer.
const BIG_MIME: &str = "application/x-clip-probe";

/// The shared-buffer path: an `OfferBuffer` is a snapshot (the owner writes
/// its pages again after offering and the paste still reads the original), a
/// paste of it reads back every byte, and a MIME the offer does not carry is
/// "not found" rather than a denial.
fn big_offer_probes(client: &clipboard::Client) -> messenger::Result<()> {
    let (handle, va, _) = sys::buffer_create(BIG_LEN as u64).map_err(Error::Errno)?;
    // SAFETY: `buffer_create` mapped at least `BIG_LEN` writable bytes at `va`,
    // owned by this task until the buffer is closed below.
    let pages = unsafe { core::slice::from_raw_parts_mut(va as *mut u8, BIG_LEN) };
    pages.fill(0xAA);
    let request = clipboard::offer_buffer_request(
        "clippaste",
        BIG_MIME,
        libmessenger::Buffer::whole(handle, BIG_LEN as u64),
    )?;
    let token = clipboard::decode_token(&client.endpoint().call(&request, None)?)?;
    // The owner keeps using its pages.
    pages.fill(0x55);
    let _ = sys::buffer_close(handle);
    let bytes = client.paste_token(token, BIG_MIME)?;
    if bytes.len() != BIG_LEN || bytes.iter().any(|&byte| byte != 0xAA) {
        fail("SNAPSHOT", "the paste is not what was copied");
    }
    sys::write_str(&format!("CLIP:SNAPSHOT:PASS bytes={}\n", bytes.len()));
    match client.paste_token(token, "image/png") {
        Err(Error::Errno(code)) if code == -errno::ENOENT => sys::write_str("CLIP:MIME:PASS\n"),
        Ok(_) => fail("MIME", "a MIME the offer lacks was served"),
        Err(error) => fail("MIME", error.message()),
    }
    Ok(())
}

/// Print `CLIP:<name>:FAIL:<detail>` and exit non-zero.
fn fail(name: &str, detail: &str) -> ! {
    trace::line!(trace::Channel::Clip, "{name}:FAIL:{detail}");
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

/// Nap one PIT tick's worth between retries ([`user::sys::nap`], a real sleep;
/// this used to park on a throwaway channel pair, since userspace had no
/// sleep call).
fn park_tick() {
    user::sys::nap();
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
