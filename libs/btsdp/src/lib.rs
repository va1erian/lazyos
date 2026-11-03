//! The host side of the Bluetooth Service Discovery Protocol for the LazyOS
//! `btd` service (`docs/bluetooth-plan.md` B3): ask a HID device for its
//! service record and read the few attributes `btd` needs.
//!
//! * [`elem`]: data elements (the SDP type-length-value encoding), parsed
//!   with an element budget.
//! * [`record`]: the HID record out of a `ServiceSearchAttributeResponse`'s
//!   attribute lists: the report descriptor (`HIDDescriptorList`), the device
//!   subclass, `HIDReconnectInitiate`, `HIDNormallyConnectable`,
//!   `HIDBootDevice`, the service name.
//! * [`Client`]: the request/response/continuation conversation, one
//!   `ServiceSearchAttributeRequest` at a time.
//!
//! The server is whatever is in radio range, so everything is bounded: the
//! bytes kept across continuations ([`MAX_ATTRIBUTE_BYTES`]), the number of
//! round trips ([`MAX_ROUNDS`]), the continuation state (16 octets, as the
//! specification caps it), the report descriptor ([`MAX_DESCRIPTOR`]) and the
//! number of elements parsed. A response that makes no progress, answers a
//! stale transaction or contradicts its own lengths ends the conversation
//! with an [`Error`].
//!
//! No clock and no I/O: the caller carries the PDUs over an L2CAP channel on
//! PSM 1 and decides how long to wait.
//!
//! Written from the Bluetooth Core Specification, Vol 3 Part B and the HID
//! profile specification; no code was taken from any other stack.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

mod client;
pub mod elem;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod record;
#[cfg(test)]
mod tests;

pub use client::{Client, Progress};
pub use record::HidRecord;

/// The L2CAP PSM of SDP.
pub const PSM_SDP: u16 = 1;
/// The most attribute-list bytes kept across all continuations.
pub const MAX_ATTRIBUTE_BYTES: usize = 16 * 1024;
/// The most responses one search may take.
pub const MAX_ROUNDS: u8 = 64;
/// The longest report descriptor accepted (the bond store's bound).
pub const MAX_DESCRIPTOR: usize = 4096;
/// The longest continuation state the specification allows.
pub const MAX_CONTINUATION: usize = 16;

/// Why a response was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The PDU ends before its layout does.
    Truncated,
    /// A length field disagrees with the bytes present, or the PDU is not
    /// the response to a search.
    BadPdu,
    /// The transaction id is not the one in flight.
    BadTransaction,
    /// The server answered with an `ErrorResponse` (the code is its own).
    Remote(u16),
    /// More attribute bytes than [`MAX_ATTRIBUTE_BYTES`].
    TooLarge,
    /// More than [`MAX_ROUNDS`] responses.
    TooManyRounds,
    /// A continuation that adds no data, or a state past the allowed size.
    NoProgress,
    /// A malformed data element (bad size, too many).
    BadElement,
    /// No HID service record with a report descriptor in the answer.
    NoHidRecord,
}
