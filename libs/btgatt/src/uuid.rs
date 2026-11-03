//! Attribute and service UUIDs as ATT carries them: 16 or 128 bits.

use alloc::vec::Vec;

/// The Bluetooth base UUID `00000000-0000-1000-8000-00805F9B34FB` in air
/// order (least significant octet first). A 128-bit UUID that is this base
/// with a 16-bit value in octets 12 and 13 *is* that 16-bit UUID.
const BASE: [u8; 16] = [
    0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uuid {
    Short(u16),
    /// A 128-bit UUID that is not a 16-bit one in disguise (air order).
    Long([u8; 16]),
}

impl Uuid {
    /// From 2 or 16 octets of air order; `None` for any other length. A
    /// 128-bit form of a 16-bit UUID is folded to [`Uuid::Short`], so two
    /// spellings of one UUID compare equal.
    pub fn from_wire(bytes: &[u8]) -> Option<Uuid> {
        match bytes.len() {
            2 => Some(Uuid::Short(u16::from_le_bytes([bytes[0], bytes[1]]))),
            16 => {
                let mut long = [0u8; 16];
                long.copy_from_slice(bytes);
                let folded = long[..12] == BASE[..12] && long[14..] == [0, 0];
                Some(if folded {
                    Uuid::Short(u16::from_le_bytes([long[12], long[13]]))
                } else {
                    Uuid::Long(long)
                })
            }
            _ => None,
        }
    }

    /// The 16-bit value, if this is a 16-bit UUID.
    pub fn short(&self) -> Option<u16> {
        match self {
            Uuid::Short(v) => Some(*v),
            Uuid::Long(_) => None,
        }
    }

    pub fn put(&self, out: &mut Vec<u8>) {
        match self {
            Uuid::Short(v) => out.extend_from_slice(&v.to_le_bytes()),
            Uuid::Long(l) => out.extend_from_slice(l),
        }
    }
}

/// GATT and HID UUIDs this crate reads.
pub mod known {
    pub const PRIMARY_SERVICE: u16 = 0x2800;
    pub const CHARACTERISTIC: u16 = 0x2803;
    pub const CCCD: u16 = 0x2902;
    pub const REPORT_REFERENCE: u16 = 0x2908;
    pub const SERVICE_HID: u16 = 0x1812;
    pub const BOOT_KEYBOARD_INPUT: u16 = 0x2A22;
    pub const BOOT_MOUSE_INPUT: u16 = 0x2A33;
    pub const HID_INFORMATION: u16 = 0x2A4A;
    pub const REPORT_MAP: u16 = 0x2A4B;
    pub const REPORT: u16 = 0x2A4D;
    pub const PROTOCOL_MODE: u16 = 0x2A4E;
}
