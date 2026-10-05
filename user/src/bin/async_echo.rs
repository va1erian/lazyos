//! `async_echo`: the futures API over `OP_CALL_BEGIN`/`OP_CALL_AWAIT`, a
//! select-style multiplexer, and `OP_CANCEL`, all in one ring-3 program
//! (issue #91).
//!
//! The default image has no service to call, so the program plays both ends:
//! it creates channel pairs, registers calls with `Call::begin` (requests are
//! in flight before anything waits), serves the requests from the local end,
//! and drives the futures. Every check prints an `ASYNC:<step>:PASS` marker on
//! the console and serial log; the run ends with `ASYNC:ALL:PASS`.
//!
//! The `ASYNC:WAIT:*` checks (`wait.rs`, issue #309) park one task on calls,
//! endpoints and a topic subscription at once through the `wait` op.
//!
//! Every non-desktop image ships it as `/system/bin/async-echo`; run it from
//! the shell and look for `ASYNC:ALL:PASS`. The scripted run is
//! `LAZYOS_CLI=1 LAZYOS_MESSENGERD=1 cargo build` (the broker for the topic
//! check, no hello window to take the keys), then
//! `tools/screenshot/examples/async_echo.json`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use core::panic::PanicInfo;
use libmessenger::{flags, Decoder, Encoder, Header, Parcel, VERSION};
use user::messenger::{self, Endpoint, Error, Result};
use user::messenger_async::{self, Call, Event, Selector};
use user::sys;

#[path = "async_echo/wait.rs"]
mod wait;

/// Interface id this demo speaks.
const IFACE: u64 = 0xE5C0_0001;
/// TLV field id of the text payload.
const FIELD_TEXT: u16 = 1;

/// Deadline for calls this program serves from its own task.
///
/// `OP_CALL_BEGIN` parks the task even though the syscall returns immediately.
/// With `None`, a single task that the timer preempts before it reaches its own
/// serve loop would sleep until a Messenger event that can no longer come. An
/// already-expired deadline makes the next tick wake it instead, so the
/// in-process demo cannot deadlock itself; a real client served by another
/// task passes `None`.
const IMMEDIATE: Option<u64> = Some(messenger::EXPIRED_DEADLINE);

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("\n");
    let code = match main() {
        Ok(()) => {
            sys::write_str("ASYNC:ALL:PASS\n");
            0
        }
        Err(()) => {
            sys::write_str("ASYNC:ALL:FAIL\n");
            1
        }
    };
    sys::exit(code)
}

fn main() -> core::result::Result<(), ()> {
    echo_check()?;
    select_check()?;
    one_way_check()?;
    cancel_check()?;
    wait::checks()
}

/// A call registered with `begin`, served from the local end, awaited with
/// `block_on`: the basic echo, and the `ASYNC:ECHO:PASS` marker.
fn echo_check() -> core::result::Result<(), ()> {
    let (client, server) = messenger::create_pair().map_err(report)?;
    let echo = Call::begin(client, request(1, "one"), IMMEDIATE).map_err(report)?;
    serve_once(server).map_err(report)?;
    let reply = messenger_async::block_on(echo).map_err(report)?;
    require("ECHO", text_of(&reply).as_deref() == Some("one"))
}

/// Two calls in flight at once, answered newest first, drained by the
/// selector in completion order.
fn select_check() -> core::result::Result<(), ()> {
    let (client_a, server_a) = messenger::create_pair().map_err(report)?;
    let (client_b, server_b) = messenger::create_pair().map_err(report)?;
    let mut selector = Selector::new();
    let first = selector
        .call(client_a, request(1, "alpha"), IMMEDIATE)
        .map_err(report)?;
    let second = selector
        .call(client_b, request(1, "beta"), IMMEDIATE)
        .map_err(report)?;
    // The server answers the second request first, so completion order differs
    // from submission order; both replies are still queued by the time the
    // selector looks.
    serve_once(server_b).map_err(report)?;
    serve_once(server_a).map_err(report)?;

    let mut replies: [Option<String>; 2] = [None, None];
    let mut seen = 0;
    while seen < 2 {
        match selector.step().map_err(report)? {
            Event::Call { index, result } => {
                replies[index] = text_of(&result.map_err(report)?);
                seen += 1;
            }
            Event::Idle => return require("SELECT", false),
            Event::Recv { .. } => return require("SELECT", false),
        }
    }
    require(
        "SELECT",
        replies[first].as_deref() == Some("alpha") && replies[second].as_deref() == Some("beta"),
    )
}

/// One-way traffic through both the non-blocking poll and the selector's
/// receive queue.
fn one_way_check() -> core::result::Result<(), ()> {
    let (client, server) = messenger::create_pair().map_err(report)?;
    client.send(&note("note-1")).map_err(report)?;
    let polled = server.poll_recv().map_err(report)?;
    let polled = polled.as_ref().and_then(|message| text_of(&message.parcel));
    require("POLL", polled.as_deref() == Some("note-1"))?;

    client.send(&note("note-2")).map_err(report)?;
    let mut selector = Selector::new();
    let queued = selector.recv(server);
    match selector.step().map_err(report)? {
        Event::Recv { index, message } if index == queued => require(
            "RECV",
            text_of(&message.parcel).as_deref() == Some("note-2"),
        ),
        _ => require("RECV", false),
    }
}

/// A call nobody serves is cancelled with `OP_CANCEL`; the awaiting poll
/// resolves with `-ECANCELED`.
fn cancel_check() -> core::result::Result<(), ()> {
    let (client, _server) = messenger::create_pair().map_err(report)?;
    let mut selector = Selector::new();
    let stuck = selector
        .call(client, request(1, "never"), IMMEDIATE)
        .map_err(report)?;
    selector.cancel(stuck).map_err(report)?;
    match selector.step().map_err(report)? {
        Event::Call { index, result } if index == stuck => {
            let cancelled = matches!(
                result,
                Err(Error::Errno(code)) if code == -messenger::errno::ECANCELED
            );
            require("CANCEL", cancelled)
        }
        _ => require("CANCEL", false),
    }
}

/// Receive one message and echo the parcel back, the way the kernel's
/// `messengerd` stub does.
pub(crate) fn serve_once(endpoint: Endpoint) -> Result<()> {
    let message = endpoint.recv(None)?;
    if let Some(txn) = message.txn {
        endpoint.reply(txn, &message.parcel)?;
    }
    Ok(())
}

/// A synchronous request parcel carrying one text field.
pub(crate) fn request(method: u32, text: &str) -> Parcel {
    parcel(method, flags::SYNC, text)
}

/// A one-way parcel carrying one text field.
pub(crate) fn note(text: &str) -> Parcel {
    parcel(1, flags::ONE_WAY, text)
}

fn parcel(method: u32, parcel_flags: u16, text: &str) -> Parcel {
    let mut body = Encoder::new();
    body.string(FIELD_TEXT, text)
        .expect("a short text field always encodes");
    Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: IFACE,
            method,
            ..Header::default()
        },
        body: body.finish(),
        ..Parcel::default()
    }
}

/// First string field of a parcel body.
pub(crate) fn text_of(parcel: &Parcel) -> Option<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.id == FIELD_TEXT {
            if let Ok(text) = field.as_str() {
                return Some(String::from(text));
            }
        }
    }
    None
}

/// Print one step marker; returns whether the step passed.
pub(crate) fn require(name: &str, passed: bool) -> core::result::Result<(), ()> {
    sys::write_str("ASYNC:");
    sys::write_str(name);
    sys::write_str(if passed { ":PASS\n" } else { ":FAIL\n" });
    if passed {
        Ok(())
    } else {
        Err(())
    }
}

/// Turn a Messenger failure into a console line, then keep the `Result<(), ()>`
/// shape the checks use.
pub(crate) fn report(error: Error) {
    sys::write_str("async_echo: ");
    sys::write_str(error.message());
    sys::write_str("\n");
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
