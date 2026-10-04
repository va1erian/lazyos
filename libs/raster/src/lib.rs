//! PWG Raster (PWG 5102.4), the page format IPP Everywhere printers take, for
//! LazyOS's print client (docs/printing-plan.md, stage P2).
//!
//! A stream is the sync word [`SYNC`] and then, per page, a 1796-byte
//! [`Header`] and the page's rows, compressed a line at a time: a line repeat
//! count, then PackBits-style runs of whole pixels. [`PageEncoder`] takes rows
//! as they are rendered and hands back the bytes so far, so a page never has
//! to exist whole in memory. [`decode`] reads a stream back; the print client
//! never needs it, the tests and the fuzz target do.

#![cfg_attr(not(any(test, feature = "fuzz")), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod decode;
mod encode;
mod header;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

#[cfg(test)]
mod tests;

pub use decode::{decode, DecodeError, Page};
pub use encode::{EncodeError, PageEncoder};
pub use header::{ColorSpace, Header, HEADER_LEN};

/// The sync word a PWG Raster stream starts with.
pub const SYNC: &[u8; 4] = b"RaS2";
