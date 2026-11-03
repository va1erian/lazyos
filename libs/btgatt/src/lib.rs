//! The host side of Bluetooth LE ATT and GATT for the LazyOS `btd` service
//! (`docs/bluetooth-plan.md` B4): an ATT client and HID over GATT (HOGP)
//! discovery.
//!
//! * [`att`]: the PDUs. Requests are built, responses are parsed against the
//!   request they answer, and every handle, length and entry is checked.
//! * [`Client`]: one request outstanding, the 30 second ATT timeout as a
//!   deadline, notifications passed up, the server's own requests answered
//!   "not supported", indications confirmed.
//! * [`Hogp`]: discovers the HID service (`0x1812`) once, at pairing: the
//!   Report Map (long reads included), the Report characteristics with their
//!   Report Reference (id and type) and CCCD, Protocol Mode, HID Information,
//!   and the Boot Keyboard/Mouse Input reports as the boot-protocol
//!   fallback (plan decision 7). On a reconnect [`Hogp::enable`] goes
//!   straight to the stored handles. Other services (Battery, GAP) are
//!   skipped.
//!
//! The server is whatever is in radio range, so everything is bounded: the
//! rounds of every discovery loop ([`hogp::MAX_ROUNDS`]), the
//! characteristics kept ([`hogp::MAX_CHARS`]), the Report Map
//! ([`hogp::MAX_REPORT_MAP`]), values by the MTU. A response outside the
//! range asked, one that does not ascend, or a length that contradicts
//! itself ends the conversation with an [`Error`] and the caller drops the
//! link. No clock, no I/O, no `unsafe`.
//!
//! Written from the Bluetooth Core Specification, Vol 3 Parts F and G, and the
//! HID over GATT Profile 1.0; no code was taken from any other stack.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod att;
pub mod client;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod hogp;
mod service;
#[cfg(test)]
mod tests;
pub mod uuid;

pub use client::{Client, Event, Received, ATT_TIMEOUT_MS};
pub use hogp::{HidService, Hogp, HogpEvent, Report, Step};
pub use uuid::Uuid;

/// The L2CAP fixed channel of ATT.
pub const CID_ATT: u16 = 4;

/// Why a conversation ended. All of them mean: drop the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A PDU that does not parse or contradicts its request.
    Malformed,
    /// A response that is not the one the request asks for.
    Unexpected,
    /// A request while another is outstanding (a caller bug).
    Busy,
    /// The client timed out or was violated earlier.
    Dead,
    /// A value longer than the MTU allows.
    TooLong,
    /// A request unanswered for 30 seconds.
    Timeout,
    /// The server refused a request this crate needs (the ATT error code).
    Att(u8),
    /// The device has no HID service.
    NoHidService,
    /// The HID service has no readable Report Map.
    NoReportMap,
    /// More than a bound allows.
    TooLarge,
    /// A discovery loop that does not end.
    TooManyRounds,
}
