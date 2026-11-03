//! Bluetooth HID for the LazyOS `btd` service (`docs/bluetooth-plan.md` B3):
//! the HIDP message framing on L2CAP PSMs `0x11` (control) and `0x13`
//! (interrupt), and the choice and use of a report decoder.
//!
//! * [`hidp`]: parse what a device sends (handshakes, control operations, DATA
//!   input reports) and build what the host sends (`SET_PROTOCOL`,
//!   `HID_CONTROL`, output reports).
//! * [`decode`]: [`decode::Plan`] picks report protocol (the report
//!   descriptor decoded by `usbhid::report`, plus the boot-layout keyboard)
//!   or boot protocol (`usbhid::boot`) from a descriptor and the SDP device
//!   subclass; a [`decode::Decoder`] turns reports into key edges and pointer
//!   output, and lets go of everything held when the link goes.
//!
//! Bluetooth HID reports are USB HID reports in a different envelope, so no
//! report logic is new here: the decoders are `usbhid`'s. Everything a device
//! sends is hostile: lengths are checked, nothing indexes unchecked, and a
//! report the layout does not own changes nothing.
//!
//! Written from the HID profile specification 1.1.1 and the HID 1.11 usage
//! tables; no code was taken from any other stack.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod decode;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod hidp;
#[cfg(test)]
mod tests;

/// The L2CAP PSM of the HID control channel.
pub const PSM_CONTROL: u16 = 0x11;
/// The L2CAP PSM of the HID interrupt channel.
pub const PSM_INTERRUPT: u16 = 0x13;

/// Why a message was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No bytes, or fewer than the message type needs.
    Truncated,
    /// A transaction type a device may not send on this channel.
    Unexpected,
}
