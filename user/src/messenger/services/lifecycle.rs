//! The service lifecycle contract (`idl/lifecycle.midl`, docs/shutdown.md):
//! the one-way `Shutdown` control message `init` sends a supervised service
//! during an orderly shutdown.
//!
//! The supervisor side is [`send_shutdown`]; a service checks every message it
//! receives with [`stop_requested`] before its own dispatch, makes its state
//! durable and exits 0. The sender is authenticated from its kernel-stamped
//! credentials, so an ordinary task cannot stop a service with a forged
//! message.

use alloc::string::String;

use libmessenger::Parcel;

use crate::messenger::{Endpoint, Error, Message, Result};
use crate::sys;

use super::header;

/// The generated `os.lazy.lifecycle.v1` stubs.
pub use messenger_generated::os_lazy_lifecycle_v1 as wire;

/// The interface id a service registers to say it serves the contract.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// The most bytes of reason text carried (the rest is dropped).
const MAX_REASON: usize = 128;

/// The one-way `Shutdown` message.
pub fn shutdown_message(reason: &str) -> Result<Parcel> {
    let body = wire::encode_shutdown_args(&wire::ShutdownArgs {
        reason: String::from(truncate(reason)),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_SHUTDOWN),
        body,
        ..Parcel::default()
    })
}

/// Send `Shutdown` to a service's endpoint. It is one-way: the service's exit
/// is the acknowledgement.
pub fn send_shutdown(endpoint: &Endpoint, reason: &str) -> Result<()> {
    endpoint.send(&shutdown_message(reason)?)
}

/// `Some(reason)` when `message` is a lifecycle `Shutdown` from a sender
/// holding `CAP_SYS_ADMIN` (the supervisor); `None` for anything else,
/// including a `Shutdown` from anyone without the capability, which the
/// service then answers like any message it does not serve.
pub fn stop_requested(message: &Message) -> Option<String> {
    if message.interface_id() != INTERFACE || message.method() != wire::METHOD_SHUTDOWN {
        return None;
    }
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).ok()?;
    if cred.caps & sys::CAP_SYS_ADMIN == 0 {
        sys::write_str("lifecycle: shutdown refused (sender lacks CAP_SYS_ADMIN)\n");
        return None;
    }
    let args = wire::decode_shutdown_args(&message.parcel.body).unwrap_or_default();
    Some(String::from(truncate(&args.reason)))
}

/// `text` cut to at most [`MAX_REASON`] bytes on a character boundary.
fn truncate(text: &str) -> &str {
    if text.len() <= MAX_REASON {
        return text;
    }
    let mut end = MAX_REASON;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
