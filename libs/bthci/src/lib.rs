//! Bluetooth HCI for the LazyOS `btd` service (`docs/bluetooth-plan.md` B0).
//!
//! * [`command`]: the commands a HID host sends (and a controller model
//!   parses), [`event`]: the events it reads, [`acl`]: ACL data headers,
//!   [`ret`]: return parameters of the Command_Complete events we use.
//! * [`flow`]: command credits ([`CommandQueue`]) and the controller's ACL
//!   buffer pool ([`AclBuffers`]).
//! * [`init`]: the controller bring-up as a pure step function, with the
//!   boot fast path (page scan, LE accept-list auto-connect) first.
//!
//! The library has no I/O and no clock. Packets are the HCI packets without a
//! transport indicator (the USB transport carries them as they are). Every
//! parser treats its input as hostile: lengths are checked before use and
//! nothing indexes unchecked.
//!
//! Written from the Bluetooth Core Specification, Vol 4 Part E and Vol 2
//! Part E; no code was taken from any other stack.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod acl;
pub mod addr;
pub mod command;
pub mod event;
pub mod flow;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod init;
pub mod opcode;
pub mod ret;
#[cfg(test)]
mod tests;
mod wire;

pub use acl::{AclHeader, Boundary};
pub use addr::BdAddr;
pub use command::Command;
pub use event::Event;
pub use flow::{AclBuffers, AclError, CommandQueue};
pub use init::{ControllerInfo, Init, InitConfig, InitError, Input, Phase, Step};

/// Why a packet was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The packet ends before its layout does.
    Truncated,
    /// A length field disagrees with the bytes present.
    BadLength,
    /// A count or length above what the specification allows.
    TooLong,
}
