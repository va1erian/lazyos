//! `async_service`: the declarative `service!` macro end to end (issue #91).
//!
//! The manifest below declares interface `0xE5C0_0002` with two methods
//! (`ping`, `echo`), a heartbeat method answered with a health parcel and
//! emitted to a configured observer every two served messages, and a shutdown
//! method that ends the serve loop gracefully. The program plays client and
//! service in one process because the default image has no service to talk to:
//!
//! 1. `ping` and `echo` calls exercise dispatch and the handler table.
//! 2. A heartbeat request is answered from the service counters, and the
//!    periodic heartbeat reaches the observer endpoint.
//! 3. The shutdown control message is answered and stops the loop.
//!
//! `ASYNC:SERVICE:PASS` marks the run.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use libmessenger::{flags, Decoder, Encoder, Header, Parcel, VERSION};
use user::messenger::{self, Error, Result};
use user::messenger_async::{self, Call, FIELD_SERVED, FIELD_SHUTTING_DOWN};
use user::service;
use user::sys;

/// Interface id this service speaks.
const IFACE: u64 = 0xE5C0_0002;
/// TLV field id of the text payload.
const FIELD_TEXT: u16 = 1;

/// Deadline for calls this program serves from its own task; see the
/// `async_echo` example for why an expired tick is used.
const IMMEDIATE: Option<u64> = Some(messenger::EXPIRED_DEADLINE);

service! {
    /// The example echo service; the clauses are the whole manifest.
    EchoService {
        concurrency: mailbox,
        interface: 0xE5C0_0002,
        methods {
            1 => ping,
            2 => echo,
        },
        heartbeat { method: 40, every: 2 },
        shutdown { method: 41 },
    }
}

/// Method 1: always answers "pong".
fn ping(_message: &user::messenger::Message) -> Result<Parcel> {
    text_parcel(1, "pong")
}

/// Method 2: echoes the request's text field.
fn echo(message: &user::messenger::Message) -> Result<Parcel> {
    let text = text_of(&message.parcel).unwrap_or_default();
    text_parcel(2, &text)
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("\n");
    let code = match main() {
        Ok(()) => {
            sys::write_str("ASYNC:SERVICE:PASS\n");
            0
        }
        Err(()) => {
            sys::write_str("ASYNC:SERVICE:FAIL\n");
            1
        }
    };
    sys::exit(code)
}

fn main() -> core::result::Result<(), ()> {
    let (client, service_end) = messenger::create_pair().map_err(report)?;
    let (service_out, observer) = messenger::create_pair().map_err(report)?;
    let mut service = EchoService::new(service_end).with_heartbeat_endpoint(Some(service_out));

    // 1. Dispatch and handler wiring.
    let ping_call = Call::begin(client, request(1, "hello"), IMMEDIATE).map_err(report)?;
    let alive = service.serve_once().map_err(report)?;
    let reply = messenger_async::block_on(ping_call).map_err(report)?;
    require(
        "DISPATCH",
        alive && text_of(&reply).as_deref() == Some("pong"),
    )?;

    let echo_call = Call::begin(client, request(2, "echo!"), IMMEDIATE).map_err(report)?;
    let alive = service.serve_once().map_err(report)?;
    let reply = messenger_async::block_on(echo_call).map_err(report)?;
    require(
        "HANDLER",
        alive && text_of(&reply).as_deref() == Some("echo!"),
    )?;

    // Two messages served is the `every: 2` heartbeat cadence.
    let beat = observer.poll_recv().map_err(report)?;
    let heartbeat_seen = beat
        .as_ref()
        .is_some_and(|message| message.parcel.header.method == 40);
    require("HEARTBEAT", heartbeat_seen)?;

    // 2. A heartbeat request is answered from the service counters.
    let health_call = Call::begin(client, request(40, ""), IMMEDIATE).map_err(report)?;
    let alive = service.serve_once().map_err(report)?;
    let reply = messenger_async::block_on(health_call).map_err(report)?;
    require(
        "HEALTH",
        alive && field_u64(&reply, FIELD_SERVED) == Some(3),
    )?;

    // 3. Graceful shutdown: the control message is answered and stops `run`.
    let bye_call = Call::begin(client, request(41, ""), IMMEDIATE).map_err(report)?;
    let alive = service.serve_once().map_err(report)?;
    let reply = messenger_async::block_on(bye_call).map_err(report)?;
    require("SHUTDOWN", !alive && service.is_shutting_down())?;
    require(
        "SHUTDOWN-REPLY",
        field_bool(&reply, FIELD_SHUTTING_DOWN) == Some(true),
    )?;

    // The retained-ish final heartbeat carries the shutdown flag.
    let final_beat = observer.poll_recv().map_err(report)?;
    let final_flag = final_beat
        .as_ref()
        .and_then(|message| field_bool(&message.parcel, FIELD_SHUTTING_DOWN));
    require("HEARTBEAT-FINAL", final_flag == Some(true))?;
    Ok(())
}

/// A synchronous request parcel carrying one text field.
fn request(method: u32, text: &str) -> Parcel {
    parcel(method, flags::SYNC, text)
}

/// A reply parcel carrying one text field.
fn text_parcel(method: u32, text: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(FIELD_TEXT, text).map_err(Error::Parcel)?;
    Ok(parcel_with(method, 0, body.finish()))
}

fn parcel(method: u32, parcel_flags: u16, text: &str) -> Parcel {
    let mut body = Encoder::new();
    body.string(FIELD_TEXT, text)
        .expect("a short text field always encodes");
    parcel_with(method, parcel_flags, body.finish())
}

fn parcel_with(method: u32, parcel_flags: u16, body: alloc::vec::Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: IFACE,
            method,
            ..Header::default()
        },
        body,
        ..Parcel::default()
    }
}

/// First string field of a parcel body.
fn text_of(parcel: &Parcel) -> Option<alloc::string::String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.id == FIELD_TEXT {
            if let Ok(text) = field.as_str() {
                return Some(alloc::string::String::from(text));
            }
        }
    }
    None
}

/// A `u64` field by id.
fn field_u64(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// A `bool` field by id.
fn field_bool(parcel: &Parcel, id: u16) -> Option<bool> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.id == id {
            return field.as_bool().ok();
        }
    }
    None
}

/// Print one step marker; returns whether the step passed.
fn require(name: &str, passed: bool) -> core::result::Result<(), ()> {
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
fn report(error: Error) {
    sys::write_str("async_service: ");
    sys::write_str(error.message());
    sys::write_str("\n");
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
