//! VFAT long file names (issue #414).
//!
//! A long name is stored as a run of attribute-`0x0F` directory slots placed
//! just before the entry's short 8.3 slot, highest sequence number first. Each
//! slot carries 13 UTF-16 units and the checksum of the short name it belongs
//! to. The bytes are untrusted, so [`LfnBuilder`] accepts a run only when it is
//! wholly consistent and otherwise hands back nothing: the caller then falls
//! back to the short name, exactly as if the run were not there.

use alloc::string::String;

/// Units per long-name slot.
const UNITS_PER_SLOT: usize = 13;
/// The highest sequence number a run may use (20 slots = 260 units).
const MAX_SLOTS: usize = 20;
/// The longest name VFAT allows, in UTF-16 units.
const MAX_NAME_UNITS: usize = 255;
/// Set on the sequence byte of the first slot stored (the last of the name).
const LAST_FLAG: u8 = 0x40;

/// The checksum a long-name slot stores for the short name it belongs to.
pub(super) fn short_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &byte| {
        (sum >> 1).wrapping_add((sum & 1) << 7).wrapping_add(byte)
    })
}

/// Whether a raw directory slot is a long-name slot.
pub(super) fn is_lfn_slot(raw: &[u8; 32]) -> bool {
    raw[11] & 0x3F == 0x0F
}

/// Reassembles the long name that precedes a short entry, one slot at a time.
pub(super) struct LfnBuilder {
    units: [u16; MAX_SLOTS * UNITS_PER_SLOT],
    /// A run has started (its first-stored slot carried the last flag).
    active: bool,
    /// Every slot so far was consistent with the run.
    valid: bool,
    /// Sequence number the next slot must carry; `0` once the run is complete.
    next: u8,
    /// Slots the run claims to have.
    total: u8,
    checksum: u8,
}

impl LfnBuilder {
    pub(super) fn new() -> LfnBuilder {
        LfnBuilder {
            units: [0; MAX_SLOTS * UNITS_PER_SLOT],
            active: false,
            valid: false,
            next: 0,
            total: 0,
            checksum: 0,
        }
    }

    /// Forget any run in progress (a deleted slot, a label, or a consumed run).
    pub(super) fn reset(&mut self) {
        self.active = false;
        self.valid = false;
        self.next = 0;
        self.total = 0;
    }

    /// Take the next long-name slot of a run.
    pub(super) fn feed(&mut self, raw: &[u8; 32]) {
        let ord = raw[0];
        let seq = ord & !LAST_FLAG;
        if ord & LAST_FLAG != 0 {
            // A new run supersedes whatever was pending.
            self.reset();
            self.active = true;
            self.valid = seq >= 1 && usize::from(seq) <= MAX_SLOTS;
            self.total = seq;
            self.next = seq;
            self.checksum = raw[13];
        }
        if !self.active || !self.valid {
            return; // an orphaned slot never starts a run
        }
        // Sequence must descend by one; the type byte and the (zero) cluster
        // field must be as the spec says; the checksum must not change.
        let consistent = self.next != 0
            && seq == self.next
            && raw[12] == 0
            && raw[26] == 0
            && raw[27] == 0
            && raw[13] == self.checksum;
        if !consistent {
            self.valid = false;
            return;
        }
        let base = (usize::from(seq) - 1) * UNITS_PER_SLOT;
        let mut unit = 0;
        for range in [1..11, 14..26, 28..32] {
            for pair in raw[range].chunks_exact(2) {
                self.units[base + unit] = u16::from_le_bytes([pair[0], pair[1]]);
                unit += 1;
            }
        }
        self.next -= 1;
    }

    /// Finish the run for the short entry `short`, returning the long name if
    /// the run is complete, matches the short name's checksum and decodes.
    /// Always leaves the builder empty.
    pub(super) fn finish(&mut self, short: &[u8; 11]) -> Option<String> {
        let ok =
            self.active && self.valid && self.next == 0 && self.checksum == short_checksum(short);
        let total = usize::from(self.total) * UNITS_PER_SLOT;
        let name = if ok { self.decode(total) } else { None };
        self.reset();
        name
    }

    /// Decode the first `total` units: stop at the NUL terminator (the rest is
    /// 0xFFFF padding) and refuse unpaired surrogates, empty or over-long
    /// names, and the characters a path component can never contain.
    fn decode(&self, total: usize) -> Option<String> {
        let units = &self.units[..total];
        let len = units.iter().position(|&unit| unit == 0).unwrap_or(total);
        if len == 0 || len > MAX_NAME_UNITS {
            return None;
        }
        let mut name = String::new();
        for decoded in char::decode_utf16(units[..len].iter().copied()) {
            let ch = decoded.ok()?;
            if ch == '/' {
                return None;
            }
            name.push(ch);
        }
        Some(name)
    }
}
