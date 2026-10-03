//! USB Mass Storage, Bulk-Only Transport (USB MSC BOT 1.0), and the SCSI
//! commands a USB stick needs (SPC-4/SBC-3 subset), for `usbd`
//! (`docs/architecture/usb-storage.md`).
//!
//! Every stick speaks BOT (interface class 08, subclass 06 "SCSI transparent",
//! protocol 50); UAS is out of scope. The library is the whole protocol:
//!
//! * [`desc`] finds a BOT interface and its bulk endpoints in a configuration
//!   descriptor chain, with the SuperSpeed companion's max burst.
//! * [`bot`] encodes the Command Block Wrapper, checks the Command Status
//!   Wrapper (signature, tag, status, residue) and runs one command through a
//!   [`bot::Pipe`] with the specification's error recovery: Clear Feature
//!   HALT after a stalled data or status stage, Reset Recovery (Bulk-Only
//!   Mass Storage Reset, then Clear Feature HALT on both bulk endpoints) after
//!   a phase error, an invalid CSW or a transport failure.
//! * [`scsi`] builds the command blocks and parses what the device returns
//!   (INQUIRY, sense data, READ CAPACITY(10)/(16), MODE SENSE(6)).
//! * [`disk`] is the block device on top: bring-up (INQUIRY, TEST UNIT READY
//!   with retry on UNIT ATTENTION and NOT READY, capacity, write protect),
//!   READ/WRITE(10) or (16) split to the transfer limit, SYNCHRONIZE CACHE.
//!
//! Everything a device sends is hostile: parsed from a copy, every length
//! checked, every loop bounded. Pure `no_std` logic with host tests against a
//! model device (`tests/model.rs`) and fuzz entry points; nothing here touches
//! a controller.

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod bot;
pub mod desc;
pub mod disk;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod scsi;
#[cfg(test)]
mod tests;

/// Why a descriptor or a device reply was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than the structure needs.
    Short,
    /// A type, signature or code field is not one this library accepts.
    Malformed,
    /// A length field disagrees with the data.
    BadLength,
}
