//! USB Audio Class playback for `usbd` (docs/usb-audio-plan.md): what a
//! device's class descriptors say, which alternate setting and rate to use,
//! how many frames each isochronous service interval carries, and how the
//! mixer's stereo `S16Le` becomes the device's samples.
//!
//! * [`desc`]: the audio functions of a configuration chain (UAC 1.0 and
//!   2.0): entities, and every playback alternate with its format, rates and
//!   endpoints. Parsed from a copy, every length checked, every loop bounded
//!   by its input.
//! * [`choose`]: the alternate and rate for a request, and the rates any
//!   alternate supports.
//! * [`packet`]: frames per service interval from the nominal rate or the
//!   device's feedback.
//! * [`convert`]: stereo `S16Le` to 1..=8 channels in 2-, 3- or 4-byte
//!   subslots.
//! * [`request`]: payloads of the class requests (rates, ranges).
//!
//! Pure `no_std` logic with host tests; nothing touches a controller.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod choose;
pub mod convert;
pub mod desc;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod packet;
pub mod request;
#[cfg(test)]
mod tests;

/// The rates the audio interface's `AudioInfo.rates` bitmap names, bit `n`
/// for entry `n` (`idl/audio.midl`, the same order as virtio-sound's).
pub const STANDARD_RATES: [u32; 11] = [
    8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000,
];

/// Why a descriptor chain was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A record's `bLength` is below 2 or runs past the chain.
    BadLength,
    /// A class request's reply is shorter than it says.
    Short,
}
