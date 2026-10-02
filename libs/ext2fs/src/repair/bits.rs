//! A fixed-size bit set whose allocation can fail without aborting: the
//! repair's per-block and per-inode marks (one bit each, so a 1 TiB volume of
//! 4 KiB blocks costs 32 MiB per set).

use alloc::vec::Vec;

use super::refuse;
use super::RepairError;

pub(super) struct Bits {
    words: Vec<u64>,
    len: u32,
}

impl Bits {
    /// `len` clear bits, or a refusal when the memory is not there.
    pub(super) fn new(len: u32) -> Result<Self, RepairError> {
        let count = (len as usize).div_ceil(64);
        let mut words = Vec::new();
        if words.try_reserve_exact(count).is_err() {
            return Err(refuse("not enough memory for the repair's bitmaps"));
        }
        words.resize(count, 0);
        Ok(Bits { words, len })
    }

    /// Bit `index` (false past the end).
    pub(super) fn get(&self, index: u32) -> bool {
        index < self.len && self.words[(index / 64) as usize] & (1 << (index % 64)) != 0
    }

    /// Set bit `index`, returning whether it was already set (an index past
    /// the end reads as already set, so a caller treats it as a conflict).
    pub(super) fn set(&mut self, index: u32) -> bool {
        if index >= self.len {
            return true;
        }
        let word = &mut self.words[(index / 64) as usize];
        let mask = 1 << (index % 64);
        let was = *word & mask != 0;
        *word |= mask;
        was
    }
}
