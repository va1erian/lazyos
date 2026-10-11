//! The firmware image (upstream `fw.h` `struct rtw_fw_hdr`,
//! `struct rtw_fw_hdr_legacy`; `mac.c` `check_firmware_size`; `main.c`
//! `__update_firmware_info`, `__update_firmware_feature`).
//!
//! `rtw88/rtw8822c_fw.bin` is a 64-byte header followed by three sections,
//! DMEM, IMEM and (when `mem_usage` bit 4 is set) EMEM, each followed by an
//! 8-byte checksum the DDMA engine verifies. The file comes from disk, so
//! every size is checked against the image before any section is sliced.

use crate::error::{Error, Result};

/// Bytes of the 3081-CPU firmware header (`FW_HDR_SIZE`).
pub const FW_HDR_SIZE: usize = 64;
/// Bytes of checksum after each section (`FW_HDR_CHKSUM_SIZE`).
pub const FW_HDR_CHKSUM_SIZE: u32 = 8;
/// Bytes of the 8051-CPU ("legacy") firmware header.
pub const FW_HDR_LEGACY_SIZE: usize = 32;

/// Feature bits (upstream `enum rtw_fw_feature`).
pub mod feature {
    pub const SIG: u32 = 1 << 0;
    pub const LPS_C2H: u32 = 1 << 1;
    pub const LCLK: u32 = 1 << 2;
    pub const PG: u32 = 1 << 3;
    pub const TX_WAKE: u32 = 1 << 4;
    pub const BCN_FILTER: u32 = 1 << 5;
    pub const NOTIFY_SCAN: u32 = 1 << 6;
    pub const ADAPTIVITY: u32 = 1 << 7;
    pub const SCAN_OFFLOAD: u32 = 1 << 8;
    /// `FW_FEATURE_EXT_OLD_PAGE_NUM` (in `feature_ext`).
    pub const EXT_OLD_PAGE_NUM: u32 = 1 << 0;
}

/// Upstream `RTW_FW_VER_CODE`.
pub const fn ver_code(ver: u16, sub_ver: u8, idx: u8) -> u32 {
    ((ver as u32) << 16) | ((sub_ver as u32) << 8) | idx as u32
}

/// The 64-byte header of a 3081-CPU firmware (upstream `struct rtw_fw_hdr`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FwHeader {
    pub signature: u16,
    pub category: u8,
    pub function: u8,
    pub version: u16,
    pub subversion: u8,
    pub subindex: u8,
    pub feature: u32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub year: u16,
    pub mem_usage: u8,
    pub h2c_fmt_ver: u16,
    pub dmem_addr: u32,
    pub dmem_size: u32,
    pub imem_size: u32,
    pub emem_size: u32,
    pub emem_addr: u32,
    pub imem_addr: u32,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl FwHeader {
    /// Read the header fields; `b` must hold at least [`FW_HDR_SIZE`] bytes.
    fn read(b: &[u8]) -> FwHeader {
        FwHeader {
            signature: le16(b, 0x00),
            category: b[0x02],
            function: b[0x03],
            version: le16(b, 0x04),
            subversion: b[0x06],
            subindex: b[0x07],
            feature: le32(b, 0x0C),
            month: b[0x10],
            day: b[0x11],
            hour: b[0x12],
            min: b[0x13],
            year: le16(b, 0x14),
            mem_usage: b[0x18],
            h2c_fmt_ver: le16(b, 0x1C),
            dmem_addr: le32(b, 0x20),
            dmem_size: le32(b, 0x24),
            imem_size: le32(b, 0x30),
            emem_size: le32(b, 0x34),
            emem_addr: le32(b, 0x38),
            imem_addr: le32(b, 0x3C),
        }
    }

    /// The EMEM section is present (`mem_usage & BIT(4)`).
    pub fn has_emem(&self) -> bool {
        self.mem_usage & (1 << 4) != 0
    }

    /// The version as one comparable number (`RTW_FW_SUIT_VER_CODE`).
    pub fn ver_code(&self) -> u32 {
        ver_code(self.version, self.subversion, self.subindex)
    }
}

/// The 32-byte header of an 8051-CPU firmware (upstream
/// `struct rtw_fw_hdr_legacy`). The 8822C never uses it; it is parsed so a
/// wrong file (an 8723D/8821C blob renamed) is reported for what it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FwHeaderLegacy {
    pub signature: u16,
    pub category: u8,
    pub function: u8,
    pub version: u16,
    pub subversion1: u8,
    pub subversion2: u8,
    pub size: u16,
}

impl FwHeaderLegacy {
    /// Parse the legacy header (no section checks: there are none in it).
    pub fn parse(b: &[u8]) -> Result<FwHeaderLegacy> {
        if b.len() < FW_HDR_LEGACY_SIZE {
            return Err(Error::BadFirmware("shorter than a legacy header"));
        }
        Ok(FwHeaderLegacy {
            signature: le16(b, 0),
            category: b[2],
            function: b[3],
            version: le16(b, 4),
            subversion1: b[6],
            subversion2: b[7],
            size: le16(b, 0x0C),
        })
    }
}

/// One section to download: where it goes and its bytes (checksum included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FwSection<'a> {
    /// Destination in the CPU's address space, `BIT(31)` cleared as upstream
    /// does before the DDMA.
    pub addr: u32,
    pub data: &'a [u8],
}

/// A validated 3081-CPU firmware image.
#[derive(Clone, Copy, Debug)]
pub struct Firmware<'a> {
    pub hdr: FwHeader,
    pub dmem: FwSection<'a>,
    pub imem: FwSection<'a>,
    pub emem: Option<FwSection<'a>>,
}

impl<'a> Firmware<'a> {
    /// Parse and validate `image` (upstream `check_firmware_size`): the
    /// header plus each section and its checksum must add up to the file's
    /// exact length.
    pub fn parse(image: &'a [u8]) -> Result<Firmware<'a>> {
        if image.len() < FW_HDR_SIZE {
            return Err(Error::BadFirmware("shorter than the header"));
        }
        let hdr = FwHeader::read(image);
        let dmem = u64::from(hdr.dmem_size) + u64::from(FW_HDR_CHKSUM_SIZE);
        let imem = u64::from(hdr.imem_size) + u64::from(FW_HDR_CHKSUM_SIZE);
        let emem = if hdr.has_emem() { u64::from(hdr.emem_size) + u64::from(FW_HDR_CHKSUM_SIZE) } else { 0 };
        let real = FW_HDR_SIZE as u64 + dmem + imem + emem;
        if real != image.len() as u64 {
            return Err(Error::BadFirmware("section sizes do not match the file"));
        }
        // All sizes are now bounded by the slice length, so they fit usize.
        let (dmem, imem, emem) = (dmem as usize, imem as usize, emem as usize);
        let d0 = FW_HDR_SIZE;
        let i0 = d0 + dmem;
        let e0 = i0 + imem;
        Ok(Firmware {
            hdr,
            dmem: FwSection { addr: hdr.dmem_addr & !(1 << 31), data: &image[d0..i0] },
            imem: FwSection { addr: hdr.imem_addr & !(1 << 31), data: &image[i0..e0] },
            emem: if hdr.has_emem() {
                Some(FwSection { addr: hdr.emem_addr & !(1 << 31), data: &image[e0..e0 + emem] })
            } else {
                None
            },
        })
    }

    /// The feature word as upstream keeps it: zero unless the image is
    /// signed (`FW_FEATURE_SIG`).
    pub fn feature(&self) -> u32 {
        if self.hdr.feature & feature::SIG != 0 {
            self.hdr.feature
        } else {
            0
        }
    }

    /// The extended features upstream derives from the version: firmware
    /// before 9.9.13 sizes its probe-request pages the old way.
    pub fn feature_ext(&self) -> u32 {
        if self.hdr.ver_code() < ver_code(9, 9, 13) {
            feature::EXT_OLD_PAGE_NUM
        } else {
            0
        }
    }
}
