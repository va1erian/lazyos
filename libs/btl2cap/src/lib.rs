//! Bluetooth L2CAP for the LazyOS `btd` service (`docs/bluetooth-plan.md` B0).
//!
//! * [`pdu`]: the basic header and the signalling commands.
//! * [`frag`]: ACL fragmentation and reassembly, bounded.
//! * [`L2cap`]: BR/EDR basic-mode connection-oriented channels (connect,
//!   configure, data, disconnect, echo, information) and the LE fixed
//!   channels: 4 (ATT) and 6 (SMP) pass through as data, 5 carries the
//!   signalling that LE peripherals use.
//!
//! The manager is a pure state machine: time is an argument, outputs are
//! [`Action`]s. It admits an incoming connection only to a PSM the upper
//! layer has allowed on that link with [`L2cap::set_allowed`], which `btd`
//! calls once the link is bonded and encrypted (decision 6 of the plan);
//! everything else gets "security block" and a [`Event::Refused`].
//!
//! Written from the Bluetooth Core Specification, Vol 3 Part A; no code was
//! taken from any other stack.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod frag;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod link;
mod manager;
pub mod pdu;
mod signal;
#[cfg(test)]
mod tests;

pub use link::{Handle, LinkKind};
pub use manager::{Action, CloseReason, Config, Event, L2cap, MAX_VIOLATIONS};

/// Why a packet or a call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncated,
    BadLength,
    TooLong,
    /// A well-formed value that is not acceptable (zero identifier, unknown
    /// handle or channel, table full).
    Invalid,
}
