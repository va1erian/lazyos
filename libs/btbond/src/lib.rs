//! The Bluetooth bond store (`docs/bluetooth-plan.md` decision 4, phase B0).
//!
//! `btd` reads this file before any session exists, so the format is
//! versioned, checksummed, and decoded from bytes nobody vouches for: a
//! truncated, bit-rotted or hand-edited file is refused with an [`Error`] and
//! never panics. This crate does **no file I/O**; `btd` reads the bytes,
//! calls [`BondStore::decode`], and writes [`BondStore::encode`] atomically
//! (`.new` plus rename). The layout is in [`codec`].
//!
//! Bonds are keyed by `(address, kind)`. Keys never appear in `Debug` output.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod bond;
pub mod codec;
mod crc;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

pub use bond::{AddrKind, Bond, Gatt, InputClass, LinkKey, Ltk, ReportHandle};
pub use bthci::BdAddr;
pub use codec::{MAX_FILE, MAX_RECORD};
pub use crc::crc32;

pub const MAGIC: [u8; 4] = *b"LZBT";
/// The only layout this crate reads. A new layout bumps this and the old one
/// is dropped, not carried (no compatibility paths).
pub const VERSION: u16 = 1;
/// Bonds one store holds.
pub const MAX_BONDS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Magic,
    Version(u16),
    Checksum,
    /// A count or length above the format's bound.
    TooLarge,
    /// Bytes left over after the last record.
    Trailing,
    /// The same `(address, kind)` twice.
    Duplicate,
    /// A field that breaks a rule of the format; the text names it.
    Invalid(&'static str),
    /// The store already holds [`MAX_BONDS`] bonds.
    Full,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BondStore {
    bonds: Vec<Bond>,
}

impl BondStore {
    pub fn new() -> BondStore {
        BondStore::default()
    }

    pub fn len(&self) -> usize {
        self.bonds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bonds.is_empty()
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Bond> {
        self.bonds.iter()
    }

    pub fn find(&self, addr: BdAddr, kind: AddrKind) -> Option<&Bond> {
        self.bonds.iter().find(|b| b.id() == (addr, kind))
    }

    /// Add a bond, or replace the one with the same identity (re-pairing).
    pub fn upsert(&mut self, bond: Bond) -> Result<(), Error> {
        bond.validate()?;
        if let Some(slot) = self.bonds.iter_mut().find(|b| b.id() == bond.id()) {
            *slot = bond;
        } else if self.bonds.len() >= MAX_BONDS {
            return Err(Error::Full);
        } else {
            self.bonds.push(bond);
        }
        Ok(())
    }

    /// Forget a device; true when it was bonded.
    pub fn remove(&mut self, addr: BdAddr, kind: AddrKind) -> bool {
        let before = self.bonds.len();
        self.bonds.retain(|b| b.id() != (addr, kind));
        self.bonds.len() != before
    }

    /// Insert while decoding: a duplicate is corruption, not an update.
    pub(crate) fn insert_unique(&mut self, bond: Bond) -> Result<(), Error> {
        if self.find(bond.addr, bond.kind).is_some() {
            return Err(Error::Duplicate);
        }
        self.bonds.push(bond);
        Ok(())
    }

    /// The bytes to write. Fails only if a bond breaks the format's rules.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        codec::encode(self)
    }

    /// Parse a file. Strict: any deviation is an error, and the store is not
    /// partially filled.
    pub fn decode(bytes: &[u8]) -> Result<BondStore, Error> {
        codec::decode(bytes)
    }
}
