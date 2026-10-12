//! GUIDs in the on-disk form: the first three fields little-endian, the last
//! eight bytes as written, so `C12A7328-F81F-11D2-...` is stored
//! `28 73 2A C1 1F F8 D2 11 ...`.

use core::fmt;

/// A GUID as its 16 on-disk bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Guid(pub [u8; 16]);

/// The EFI System Partition.
pub const ESP: Guid = Guid::from_text("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
/// Microsoft basic data (FAT or NTFS; what a GPT stick's FAT partition is).
pub const BASIC_DATA: Guid = Guid::from_text("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
/// Linux filesystem data (what `fdisk`/`gdisk` give a new partition).
pub const LINUX_DATA: Guid = Guid::from_text("0FC63DAF-8483-4772-8E79-3D69D8477DE4");
/// Linux root (x86-64), discoverable-partitions spec.
pub const LINUX_ROOT_X86_64: Guid = Guid::from_text("4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709");
/// Linux `/home`, discoverable-partitions spec.
pub const LINUX_HOME: Guid = Guid::from_text("933AC7E1-2EB4-4F13-B844-0E14E2AEF915");

impl Guid {
    /// The all-zero GUID, which marks an unused entry.
    pub const ZERO: Guid = Guid([0; 16]);

    /// Parse the 36-character `8-4-4-4-12` text form at compile time; a
    /// malformed literal fails the build.
    pub const fn from_text(text: &str) -> Guid {
        match Guid::parse(text) {
            Some(guid) => guid,
            None => panic!("malformed GUID literal"),
        }
    }

    /// Parse the 36-character `8-4-4-4-12` text form (either case).
    pub const fn parse(text: &str) -> Option<Guid> {
        let text = text.as_bytes();
        if text.len() != 36 || text[8] != b'-' || text[13] != b'-' || text[18] != b'-' {
            return None;
        }
        if text[23] != b'-' {
            return None;
        }
        // Text byte offsets of the 16 values, in text order.
        const AT: [usize; 16] = [0, 2, 4, 6, 9, 11, 14, 16, 19, 21, 24, 26, 28, 30, 32, 34];
        // Where text-order byte i lands on disk.
        const TO: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
        let mut out = [0u8; 16];
        let mut i = 0;
        while i < 16 {
            let (Some(hi), Some(lo)) = (hex(text[AT[i]]), hex(text[AT[i] + 1])) else {
                return None;
            };
            out[TO[i]] = hi << 4 | lo;
            i += 1;
        }
        Some(Guid(out))
    }

    pub fn is_zero(&self) -> bool {
        *self == Guid::ZERO
    }

    /// A short name for the types LazyOS knows, for logs.
    pub fn type_name(&self) -> Option<&'static str> {
        Some(match *self {
            ESP => "esp",
            BASIC_DATA => "basic-data",
            LINUX_DATA => "linux-data",
            LINUX_ROOT_X86_64 => "linux-root",
            LINUX_HOME => "linux-home",
            _ => return None,
        })
    }
}

const fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Guid {
    /// The upper-case text form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9]
        )?;
        b[10..].iter().try_for_each(|byte| write!(f, "{byte:02X}"))
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
