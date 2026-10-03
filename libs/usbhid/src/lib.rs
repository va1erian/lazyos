//! USB descriptors and HID boot-protocol reports for `usbd`
//! (`docs/usb-hid-plan.md`, phase U0).
//!
//! Everything here reads bytes a device sent, and a device may be hostile (a
//! malicious stick is a USB device too), so every length is checked before
//! use, every loop is bounded by the input, and malformed input is an
//! [`Error`] or a counted, dropped record, never a panic or an out-of-bounds
//! read.
//!
//! * [`desc`] parses the device and configuration descriptors: every
//!   interface with its endpoints, and the boot-capable HID interfaces.
//! * [`hub`] parses hub descriptors, port status words and the
//!   status-change bitmap (USB 2 and SuperSpeed hubs).
//! * [`boot`] turns successive boot keyboard and mouse reports into key and
//!   button edges (a report is a *state*; the bus carries *edges*).
//! * [`report`] reads enough of a HID report descriptor to find a pointer's
//!   X, Y, wheel and buttons (a tablet has no boot protocol), and reads them
//!   out of its reports.
//!
//! Pure `no_std` logic with host tests; nothing touches a controller.

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod boot;
pub mod desc;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod hub;
pub mod report;
#[cfg(test)]
mod tests;

/// Why a descriptor was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than the descriptor needs.
    Short,
    /// The type byte is not the one expected here.
    WrongType,
    /// A length field disagrees with the data (zero, too small, or past the
    /// end of the buffer).
    BadLength,
}
