//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/btbond.rs`).
//!
//! Random bytes almost never carry a valid CRC, so [`run`] feeds the decoder
//! three forms of the input: verbatim; with the trailing four bytes replaced by
//! the right CRC; and wrapped as the body of a version-1 file (magic and
//! version in front, CRC behind). Whatever decodes must encode back to exactly
//! the same bytes (the format is canonical), and decode again to the same
//! store.

use std::vec::Vec;

use crate::{crc32, BondStore, MAGIC, VERSION};

pub fn run(data: &[u8]) {
    check(data);
    if data.len() >= 4 {
        let mut sealed = data.to_vec();
        seal(&mut sealed);
        check(&sealed);
    }
    let mut wrapped = Vec::new();
    wrapped.extend_from_slice(&MAGIC);
    wrapped.extend_from_slice(&VERSION.to_le_bytes());
    wrapped.extend_from_slice(data);
    wrapped.extend_from_slice(&[0; 4]);
    seal(&mut wrapped);
    check(&wrapped);
}

/// Overwrite the last four bytes with the CRC of the rest.
pub fn seal(bytes: &mut [u8]) {
    let n = bytes.len().saturating_sub(4);
    let crc = crc32(&bytes[..n]);
    bytes[n..].copy_from_slice(&crc.to_le_bytes());
}

fn check(bytes: &[u8]) {
    if let Ok(store) = BondStore::decode(bytes) {
        let again = store.encode().expect("a decoded store encodes");
        assert_eq!(again, bytes, "the format is canonical");
        assert_eq!(BondStore::decode(&again), Ok(store));
    }
}
