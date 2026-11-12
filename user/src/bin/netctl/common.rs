//! Helpers shared by `netctl`'s modes: connecting to the stack service,
//! sleeping, waiting for an address, and formatting.

use alloc::format;
use alloc::string::String;

use libmessenger::client::{CallError, Connection};
use libmessenger::Parcel;
use user::messenger::client::{call_error, Native};
use user::messenger::netstack::Client;
use user::messenger::{Endpoint, Error as MsgError};
use user::sys;

/// Ticks to wait for an address after a renewal or a reattach.
const ADDRESS_TICKS: u64 = 1500;

/// Sleep one PIT tick ([`sys::nap`]).
pub(super) fn nap() {
    sys::nap();
}

/// Prefix a call failure with what was being attempted.
pub(super) fn fail(what: &str) -> impl Fn(CallError) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

/// [`fail`] for a local Messenger operation rather than a call.
pub(super) fn local(what: &str) -> impl Fn(MsgError) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

/// Whether `result` failed with exactly `errno` (a positive errno value).
pub(super) fn is_errno<T>(result: &Result<T, CallError>, errno: i64) -> bool {
    matches!(result, Err(error) if error.code() == -errno)
}

/// Whether `result` failed at all (any error).
pub(super) fn failed<T>(result: &Result<T, CallError>) -> bool {
    result.is_err()
}

// The probes below send requests by hand on purpose: they must be able to
// send what the generated client never would (a foreign interface id, an
// unknown method, a body that is not a request, an `ALLOW_NESTED` call that
// parks, a request the kernel drops), so they go through the connection's
// handle instead of through `wire::Client`.

/// The service's endpoint, resolved by the connection.
pub(super) fn endpoint_of(conn: &Connection<Native>) -> Result<Endpoint, CallError> {
    conn.connect()
        .map(Endpoint::from_raw)
        .map_err(CallError::Transport)
}

/// One hand-built request and its reply, a failure the service reports as the
/// error field being the error. `deadline` is an absolute tick (`None`: wait).
pub(super) fn send_parcel(
    conn: &Connection<Native>,
    request: &Parcel,
    deadline: Option<u64>,
) -> Result<Parcel, CallError> {
    let reply = endpoint_of(conn)?
        .call(request, deadline)
        .map_err(call_error)?;
    Connection::<Native>::check(reply)
}

/// Whether the reply to a parked call (`Endpoint::await_reply`) is the
/// service's refusal with `errno`.
pub(super) fn answered_with(reply: Result<Parcel, MsgError>, errno: i64) -> bool {
    let checked = reply
        .map_err(call_error)
        .and_then(Connection::<Native>::check);
    is_errno(&checked, errno)
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

/// `10.0.2.15` (or `?` for a wrong-sized field).
pub(super) fn ip_text(addr: &[u8]) -> String {
    match addr {
        [a, b, c, d] => format!("{a}.{b}.{c}.{d}"),
        _ => String::from("?"),
    }
}

/// Resolve the stack service, retrying while it is still starting up.
pub(super) fn connect() -> Result<Client, String> {
    Client::connect(Native).map_err(|error| format!("no network stack: {}", error.message()))
}

/// Wait until the stack reports an address; returns it.
pub(super) fn wait_for_address(client: &Client) -> Result<[u8; 4], String> {
    let deadline = sys::clock() + ADDRESS_TICKS;
    loop {
        let list = client.addresses().map_err(fail("addresses"))?;
        if let Some(a) = list.first() {
            if let Ok(addr) = <[u8; 4]>::try_from(a.addr.as_slice()) {
                return Ok(addr);
            }
        }
        if sys::clock() >= deadline {
            return Err(String::from("no address after waiting"));
        }
        nap();
    }
}
