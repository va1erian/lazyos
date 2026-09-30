//! Helpers shared by `netctl`'s modes: connecting to the stack service,
//! sleeping, waiting for an address, and formatting.

use alloc::format;
use alloc::string::String;

use user::messenger::netstack::Client;
use user::messenger::Error as MsgError;
use user::sys;

/// Ticks (100 Hz) to wait for `netd` to register at boot.
const CONNECT_TICKS: u64 = 1000;
/// Ticks to wait for an address after a renewal or a reattach.
const ADDRESS_TICKS: u64 = 1500;

/// Sleep one PIT tick (`wait` doubles as a timer when there is no child).
pub(super) fn nap() {
    let _ = sys::wait(sys::clock() + 1);
}

/// Prefix a Messenger failure with what was being attempted.
pub(super) fn fail(what: &str) -> impl Fn(MsgError) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

/// Whether `result` failed with exactly `errno` (a positive errno value).
pub(super) fn is_errno<T>(result: &Result<T, MsgError>, errno: i64) -> bool {
    matches!(result, Err(MsgError::Errno(code)) if *code == -errno)
}

/// Whether `result` failed at all (any error).
pub(super) fn failed<T>(result: &Result<T, MsgError>) -> bool {
    result.is_err()
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
    let deadline = sys::clock() + CONNECT_TICKS;
    loop {
        match Client::connect() {
            Ok(client) => return Ok(client),
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no network stack: {}", error.message()))
            }
            Err(_) => nap(),
        }
    }
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
