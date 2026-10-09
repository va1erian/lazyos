//! IDENTIFY DEVICE data (ACS-3 section 7.12): the 256 words a disk returns.
//! Everything in it is untrusted: a word only counts when the validity bits
//! that guard it say so, and the capacity is checked against LBA48's range.

/// Bytes of IDENTIFY data.
pub const IDENTIFY_BYTES: usize = 512;
/// Most sectors an LBA48 disk can have.
pub const MAX_LBA48_SECTORS: u64 = 1 << 48;

/// A trimmed ASCII string from IDENTIFY (at most 40 bytes, printable).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Text {
    bytes: [u8; 40],
    len: usize,
}

impl Text {
    fn parse(words: &[u16]) -> Text {
        let mut text = Text {
            bytes: [0; 40],
            len: 0,
        };
        // Each word holds two characters, the first in the high byte.
        for &word in words {
            for byte in word.to_be_bytes() {
                let byte = if (0x20..0x7F).contains(&byte) {
                    byte
                } else {
                    b' '
                };
                text.bytes[text.len] = byte;
                text.len += 1;
            }
        }
        while text.len > 0 && text.bytes[text.len - 1] == b' ' {
            text.len -= 1;
        }
        text
    }

    pub fn as_str(&self) -> &str {
        // Only printable ASCII was stored.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Debug for Text {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// Why a disk was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Word 0 says this is not an ATA disk (or the data is all ones/zeros).
    NotAta,
    /// No LBA48 (a disk under 128 GiB, or one that does not say).
    NoLba48,
    /// Zero sectors, or more than LBA48 can address.
    BadCapacity,
    /// Logical sector other than 512 bytes.
    SectorSize(u32),
    /// No `FLUSH CACHE EXT`: a write cache that cannot be made durable.
    NoFlush,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Refusal::NotAta => write!(f, "not an ATA disk"),
            Refusal::NoLba48 => write!(f, "no 48-bit LBA"),
            Refusal::BadCapacity => write!(f, "capacity out of range"),
            Refusal::SectorSize(bytes) => {
                write!(f, "{bytes}-byte logical sectors (only 512 are served)")
            }
            Refusal::NoFlush => write!(f, "no FLUSH CACHE EXT"),
        }
    }
}

/// What the driver keeps of IDENTIFY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disk {
    pub model: Text,
    pub serial: Text,
    pub firmware: Text,
    /// Addressable logical sectors (512 bytes each).
    pub sectors: u64,
    /// Bytes per physical sector (512 or more).
    pub physical_bytes: u32,
    /// The volatile write cache is enabled.
    pub write_cache: bool,
}

impl Disk {
    /// A disk with nothing known yet (a port still opening).
    pub fn unidentified() -> Disk {
        let empty = Text::parse(&[]);
        Disk {
            model: empty,
            serial: empty,
            firmware: empty,
            sectors: 0,
            physical_bytes: 512,
            write_cache: false,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.sectors * 512
    }

    /// Parse and validate the 512 bytes of IDENTIFY DEVICE data.
    pub fn parse(raw: &[u8; IDENTIFY_BYTES]) -> Result<Disk, Refusal> {
        let mut words = [0u16; 256];
        for (word, pair) in words.iter_mut().zip(raw.as_chunks::<2>().0) {
            *word = u16::from_le_bytes([pair[0], pair[1]]);
        }
        // Word 0 bit 15 is 0 for an ATA device; all ones is a dead bus.
        if words[0] & 0x8000 != 0 || words.iter().all(|&word| word == 0 || word == 0xFFFF) {
            return Err(Refusal::NotAta);
        }
        // Word 83: bits 15:14 = 01 mark it valid; bit 10 LBA48, bit 13
        // FLUSH CACHE EXT.
        let supported = words[83];
        let valid = supported >> 14 == 0b01;
        if !valid || supported & (1 << 10) == 0 {
            return Err(Refusal::NoLba48);
        }
        if supported & (1 << 13) == 0 {
            return Err(Refusal::NoFlush);
        }
        let capacity = u64::from(words[100])
            | u64::from(words[101]) << 16
            | u64::from(words[102]) << 32
            | u64::from(words[103]) << 48;
        if capacity == 0 || capacity >= MAX_LBA48_SECTORS {
            return Err(Refusal::BadCapacity);
        }
        // Word 106: bits 15:14 = 01 mark it valid. Bit 12: the logical
        // sector size is in words 117..=118 (in words), else 256 words.
        let layout = words[106];
        let mut logical = 512u32;
        let mut physical = 512u32;
        if layout >> 14 == 0b01 {
            if layout & (1 << 12) != 0 {
                let sector_words = u32::from(words[117]) | u32::from(words[118]) << 16;
                logical = sector_words.saturating_mul(2);
            }
            if layout & (1 << 13) != 0 {
                let shift = u32::from(layout & 0xF);
                // 2^shift logical sectors per physical one; 512e is 3.
                physical = logical.saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX));
            } else {
                physical = logical;
            }
        }
        if logical != 512 {
            return Err(Refusal::SectorSize(logical));
        }
        // Word 85 bit 5: write cache enabled, valid when word 84 says so.
        let enabled = words[85];
        let write_cache = words[84] >> 14 == 0b01 && enabled & (1 << 5) != 0;
        Ok(Disk {
            model: Text::parse(&words[27..47]),
            serial: Text::parse(&words[10..20]),
            firmware: Text::parse(&words[23..27]),
            sectors: capacity,
            physical_bytes: physical,
            write_cache,
        })
    }
}
