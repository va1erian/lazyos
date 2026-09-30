//! VFAT long-file-name assembly.
//!
//! A long name is stored as a run of `0x0F` directory entries placed just
//! before the short entry they belong to, last fragment first. Every byte is
//! untrusted: the run is only used when its sequence numbers descend cleanly
//! to 1 and its checksum matches the short name that follows; on any
//! inconsistency the run is dropped and the caller falls back to the short
//! name.

use alloc::string::String;

/// UTF-16 units held by one fragment.
const UNITS_PER_ENTRY: usize = 13;
/// A name is at most 255 characters, which needs 20 fragments.
const MAX_FRAGMENTS: usize = 20;
/// Longest name in characters.
pub(super) const MAX_NAME_CHARS: usize = 255;
/// Sequence flag on the first-stored (last-in-name) fragment.
const LAST_FLAG: u8 = 0x40;

/// The short-name checksum every fragment of a run must carry.
pub(super) fn checksum(short: &[u8]) -> u8 {
    short.iter().fold(0u8, |sum, &byte| {
        (if sum & 1 != 0 { 0x80u8 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(byte)
    })
}

/// Collects the fragments preceding one short entry.
pub(super) struct LfnRun {
    units: [u16; MAX_FRAGMENTS * UNITS_PER_ENTRY],
    /// Sequence number of the last accepted fragment (`0` = no run).
    expected: u8,
    /// Fragments in the run, as announced by the first one.
    total: u8,
    checksum: u8,
}

impl LfnRun {
    pub(super) const fn new() -> Self {
        LfnRun {
            units: [0; MAX_FRAGMENTS * UNITS_PER_ENTRY],
            expected: 0,
            total: 0,
            checksum: 0,
        }
    }

    /// Drop any partial run (a deleted entry, a label, an orphan).
    pub(super) fn reset(&mut self) {
        self.expected = 0;
        self.total = 0;
    }

    /// Feed one `0x0F` directory entry.
    pub(super) fn push(&mut self, raw: &[u8]) {
        let seq = raw[0];
        let ord = seq & 0x1F;
        if seq & LAST_FLAG != 0 {
            // A first fragment always starts a fresh run, orphaning any
            // earlier one.
            if ord == 0 || ord as usize > MAX_FRAGMENTS {
                return self.reset();
            }
            self.total = ord;
            self.checksum = raw[13];
        } else if self.expected < 2 || ord != self.expected - 1 || raw[13] != self.checksum {
            return self.reset();
        }
        self.expected = ord;
        let base = (ord as usize - 1) * UNITS_PER_ENTRY;
        let offsets = (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2));
        for (slot, at) in self.units[base..base + UNITS_PER_ENTRY]
            .iter_mut()
            .zip(offsets)
        {
            *slot = u16::from_le_bytes([raw[at], raw[at + 1]]);
        }
    }

    /// Finish the run against the short entry that follows it. `None` means
    /// there was no run or it was inconsistent: use the short name.
    pub(super) fn take(&mut self, short: &[u8]) -> Option<String> {
        let valid = self.expected == 1 && self.checksum == checksum(short);
        let total = self.total as usize;
        self.reset();
        if valid {
            decode(&self.units[..total * UNITS_PER_ENTRY])
        } else {
            None
        }
    }
}

/// Decode the assembled units: an optional NUL terminator followed only by
/// `0xFFFF` padding, valid UTF-16, no `/` and no NUL inside the name.
fn decode(units: &[u16]) -> Option<String> {
    let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    if units[len..].iter().skip(1).any(|&u| u != 0xFFFF) {
        return None;
    }
    let mut name = String::new();
    let mut chars = 0usize;
    for ch in char::decode_utf16(units[..len].iter().copied()) {
        let ch = ch.ok()?;
        chars += 1;
        if ch == '/' || chars > MAX_NAME_CHARS {
            return None;
        }
        name.push(ch);
    }
    (!name.is_empty()).then_some(name)
}
