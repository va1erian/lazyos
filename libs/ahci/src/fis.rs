//! The Register Host-to-Device FIS (Serial ATA 3.x section 10.3.4, AHCI
//! 1.3.1 section 10.3.1) and the ATA command opcodes the driver sends.

/// Bytes the driver writes for a command FIS (five dwords).
pub const H2D_BYTES: usize = 20;
/// The received-FIS area a port needs (AHCI 1.3.1 section 4.2.1).
pub const RECEIVED_FIS_BYTES: usize = 256;

pub const FIS_TYPE_H2D: u8 = 0x27;
/// "This FIS carries a command" bit of byte 1.
pub const FIS_COMMAND: u8 = 0x80;
/// `device` register value for LBA addressing.
pub const DEVICE_LBA: u8 = 0x40;

/// ATA opcodes (ACS-3 section 7).
pub mod ata {
    pub const READ_DMA_EXT: u8 = 0x25;
    pub const WRITE_DMA_EXT: u8 = 0x35;
    pub const FLUSH_CACHE_EXT: u8 = 0xEA;
    pub const IDENTIFY_DEVICE: u8 = 0xEC;
    pub const STANDBY_IMMEDIATE: u8 = 0xE0;
}

/// A command as the host describes it; [`H2d::encode`] lays out the FIS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct H2d {
    pub command: u8,
    /// 48-bit LBA (higher bits are ignored).
    pub lba: u64,
    /// Sector count; `0` means 65536 for the EXT commands.
    pub count: u16,
    pub device: u8,
}

impl H2d {
    /// A command with no data and no address.
    pub fn plain(command: u8) -> H2d {
        H2d {
            command,
            ..H2d::default()
        }
    }

    /// A 48-bit LBA command (`READ/WRITE DMA EXT`).
    pub fn lba48(command: u8, lba: u64, count: u16) -> H2d {
        H2d {
            command,
            lba: lba & 0xFFFF_FFFF_FFFF,
            count,
            device: DEVICE_LBA,
        }
    }

    pub fn encode(&self) -> [u8; H2D_BYTES] {
        let lba = self.lba.to_le_bytes();
        let count = self.count.to_le_bytes();
        [
            FIS_TYPE_H2D,
            FIS_COMMAND,
            self.command,
            0, // features (7:0)
            lba[0],
            lba[1],
            lba[2],
            self.device,
            lba[3],
            lba[4],
            lba[5],
            0, // features (15:8)
            count[0],
            count[1],
            0, // icc
            0, // control
            0,
            0,
            0,
            0,
        ]
    }

    pub fn decode(raw: &[u8; H2D_BYTES]) -> Option<H2d> {
        if raw[0] != FIS_TYPE_H2D || raw[1] & FIS_COMMAND == 0 {
            return None;
        }
        let mut lba = [0u8; 8];
        lba[..3].copy_from_slice(&raw[4..7]);
        lba[3..6].copy_from_slice(&raw[8..11]);
        Some(H2d {
            command: raw[2],
            lba: u64::from_le_bytes(lba),
            count: u16::from_le_bytes([raw[12], raw[13]]),
            device: raw[7],
        })
    }
}
