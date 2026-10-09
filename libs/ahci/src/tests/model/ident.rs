//! What the model's disk answers to IDENTIFY DEVICE.

use std::vec;

/// What IDENTIFY says (see `identify::Disk::parse`).
#[derive(Clone, Copy)]
pub struct Ident {
    pub lba48: bool,
    pub flush_ext: bool,
    pub sector_bytes: u32,
    pub physical_shift: u32,
    pub write_cache: bool,
    pub zero_capacity: bool,
    pub garbage: bool,
}

impl Default for Ident {
    fn default() -> Ident {
        Ident {
            lba48: true,
            flush_ext: true,
            sector_bytes: 512,
            physical_shift: 0,
            write_cache: true,
            zero_capacity: false,
            garbage: false,
        }
    }
}

pub(super) fn identify_data(ident: &Ident, sectors: u64) -> [u8; 512] {
    let mut raw = [0u8; 512];
    if ident.garbage {
        for (index, byte) in raw.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(131).wrapping_add(7);
        }
        raw[1] &= 0x7F;
        return raw;
    }
    // ATA strings: two characters per word, the first in the high byte.
    let mut text = |first: usize, words: usize, value: &str| {
        let mut bytes = vec![b' '; words * 2];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        for (index, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let at = (first + index) * 2;
            raw[at] = pair[1];
            raw[at + 1] = pair[0];
        }
    };
    text(10, 10, "SERIAL42");
    text(23, 4, "FW1.0");
    text(27, 20, "Model SSD 512GB");
    let mut put = |word: usize, value: u16| {
        raw[word * 2..word * 2 + 2].copy_from_slice(&value.to_le_bytes());
    };
    let mut w83 = 0x4000u16;
    if ident.lba48 {
        w83 |= 1 << 10;
    }
    if ident.flush_ext {
        w83 |= 1 << 13;
    }
    put(83, w83);
    put(84, 0x4000);
    put(85, if ident.write_cache { 1 << 5 } else { 0 });
    let capacity = if ident.zero_capacity { 0 } else { sectors };
    for word in 0..4 {
        put(100 + word, (capacity >> (16 * word)) as u16);
    }
    let mut w106 = 0x4000u16;
    if ident.physical_shift > 0 {
        w106 |= 1 << 13 | ident.physical_shift as u16;
    }
    if ident.sector_bytes != 512 {
        w106 |= 1 << 12;
        let words = ident.sector_bytes / 2;
        put(117, words as u16);
        put(118, (words >> 16) as u16);
    }
    put(106, w106);
    raw
}
