//! Client and wire shapes for `clipboardd` (issue #115). See the module doc
//! on [`crate::messenger::clipboard`] for the offer/request/serialize model
//! and the policy pseudo-interfaces.
//!
//! Split into [`protocol`] (parcel encode/decode) and [`client`] ([`Client`]);
//! both are re-exported here so callers keep using `clipboard::*`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Header, Parcel, VERSION};

mod client;
mod protocol;

pub use client::*;
pub use protocol::*;

/// The service's registered name.
pub const NAME: &str = "os.lazy.clipboard";

/// The generated `os.lazy.clipboard.v1` stubs (`idl/clipboard.midl`).
pub use messenger_generated::os_lazy_clipboard_v1 as wire;

/// Control interface id (`os.lazy.clipboard.v1`, from the IDL): carries
/// `Ping` and `Current`.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// ACL scope interface an `Offer` call carries:
/// `fnv1a64("os.lazy.clipboard.write.v1")` (the topics convention). A scope
/// is a capability name, not a wire field, so it is not in the IDL.
pub const WRITE_INTERFACE: u64 = fnv1a64("os.lazy.clipboard.write.v1");

/// Policy pseudo-interface a `Request` call carries:
/// `fnv1a64("os.lazy.clipboard.read.v1")`.
pub const READ_INTERFACE: u64 = fnv1a64("os.lazy.clipboard.read.v1");

/// Interface the owner of a lazy offer serves for [`method::SERIALIZE`].
pub const OWNER_INTERFACE: u64 = u64::from_le_bytes(*b"os.owner");

/// FNV-1a 64, the `tools/midlc` interface-id hash, so policy can key the
/// two pseudo-interfaces on the same value the kernel checks.
const fn fnv1a64(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0000_0100_0000_01B3);
        index += 1;
    }
    hash
}

/// Method ids, aliased from the generated stubs. `OFFER` travels on
/// [`WRITE_INTERFACE`], `REQUEST` on [`READ_INTERFACE`], `SERIALIZE` on
/// [`OWNER_INTERFACE`]; `PING` and `CURRENT` are the control interface.
pub mod method {
    use super::wire;

    /// Write: publish typed payloads for the caller's session.
    pub const OFFER: u32 = wire::METHOD_OFFER;
    /// Read: fetch a payload by token and MIME.
    pub const REQUEST: u32 = wire::METHOD_REQUEST;
    /// Owner: serialize one MIME of an offer on demand (lazy transfer).
    pub const SERIALIZE: u32 = wire::METHOD_SERIALIZE;
    /// Control: liveness probe.
    pub const PING: u32 = wire::METHOD_PING;
    /// Control: current-offer metadata, never content.
    pub const CURRENT: u32 = wire::METHOD_CURRENT;
}

/// Longest MIME string the service accepts.
pub const MAX_MIME: usize = 64;
/// Most MIME types in one offer.
pub const MAX_MIMES: usize = 8;
/// Largest inline payload the service keeps for one offer.
pub const MAX_DATA: usize = 8 * 1024;
/// Longest owner label or sink name.
pub const MAX_TEXT: usize = 128;

/// Metadata for one live offer: identity and MIME types, never content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OfferInfo {
    /// Offer token clients pass back in a `Request`.
    pub token: u64,
    /// Human-readable owner label supplied at offer time.
    pub owner: String,
    /// Session the offer belongs to (kernel-stamped).
    pub session: u64,
    /// MIME types the offer carries.
    pub mimes: Vec<String>,
    /// Whether the payload is materialized lazily by the owner.
    pub lazy: bool,
    /// Tick the offer was made.
    pub tick: u64,
}

/// The payload a `Request` yields. The kernel's `SHARE_ONLY` shared-buffer
/// object is the future home of `bytes`; until the userspace mapping
/// syscall lands the bytes ride in the reply parcel (see module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BufferHandle {
    /// Token of the offer that was read.
    pub token: u64,
    /// MIME type that was read.
    pub mime: String,
    /// Whether the bytes came from the owner's `Serialize` callback.
    pub lazy: bool,
    /// The payload bytes.
    pub bytes: Vec<u8>,
}

/// A decoded `Offer` request (the service's view).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OfferRequest {
    /// Human-readable owner label.
    pub owner: String,
    /// Owner callback endpoint name for a lazy offer.
    pub sink: Option<String>,
    /// MIME types offered.
    pub mimes: Vec<String>,
    /// Inline payloads for an eager offer.
    pub data: Vec<(String, Vec<u8>)>,
}

/// The scoped retained topic a session's paste UIs watch
/// (`docs/messenger.md` section 19).
pub fn changes_topic(session: u64) -> String {
    format!("session/{session}/clipboard/changed")
}

/// A header for a clipboard parcel on `interface_id`.
///
/// `ALLOW_NESTED` is required: every client resolves the same service
/// endpoint, and a paste by one task can overlap an offer by another, so
/// the kernel's per-channel cycle check would otherwise refuse the second
/// call with `-EDEADLK`. The service answers each request before servicing
/// the next and only calls out on a *different* channel (the owner's
/// `Serialize`), so nesting cannot form a cycle here.
fn header(interface_id: u64, method: u32) -> Header {
    Header {
        version: VERSION,
        flags: libmessenger::flags::ALLOW_NESTED,
        interface_id,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// Wrap an encoded body in a clipboard parcel.
fn parcel(interface_id: u64, method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: header(interface_id, method),
        body,
        ..Parcel::default()
    }
}
