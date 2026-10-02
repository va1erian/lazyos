//! The SCSI commands a USB stick needs (SPC-4, SBC-3) and parsers for what
//! the device returns. Command blocks are big-endian; every parser works on
//! a copy and checks its length first.

use crate::Error;

/// Operation codes.
pub mod op {
    pub const TEST_UNIT_READY: u8 = 0x00;
    pub const REQUEST_SENSE: u8 = 0x03;
    pub const INQUIRY: u8 = 0x12;
    pub const MODE_SENSE_6: u8 = 0x1A;
    pub const READ_CAPACITY_10: u8 = 0x25;
    pub const READ_10: u8 = 0x28;
    pub const WRITE_10: u8 = 0x2A;
    pub const SYNCHRONIZE_CACHE_10: u8 = 0x35;
    pub const READ_16: u8 = 0x88;
    pub const WRITE_16: u8 = 0x8A;
    pub const SERVICE_ACTION_IN_16: u8 = 0x9E;
    /// The SERVICE ACTION IN(16) action of READ CAPACITY(16).
    pub const READ_CAPACITY_16: u8 = 0x10;
}

/// Bytes asked for by each data-in command.
pub const INQUIRY_LEN: u8 = 36;
pub const SENSE_LEN: u8 = 18;
pub const CAPACITY_10_LEN: usize = 8;
pub const CAPACITY_16_LEN: u8 = 32;
pub const MODE_SENSE_LEN: u8 = 192;

/// A command block and its length (6, 10 or 16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cdb {
    bytes: [u8; 16],
    len: usize,
}

impl Cdb {
    fn new(bytes: &[u8]) -> Cdb {
        let mut cdb = Cdb {
            bytes: [0; 16],
            len: bytes.len(),
        };
        cdb.bytes[..bytes.len()].copy_from_slice(bytes);
        cdb
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

pub fn test_unit_ready() -> Cdb {
    Cdb::new(&[op::TEST_UNIT_READY, 0, 0, 0, 0, 0])
}

pub fn request_sense() -> Cdb {
    Cdb::new(&[op::REQUEST_SENSE, 0, 0, 0, SENSE_LEN, 0])
}

pub fn inquiry() -> Cdb {
    Cdb::new(&[op::INQUIRY, 0, 0, 0, INQUIRY_LEN, 0])
}

pub fn read_capacity_10() -> Cdb {
    Cdb::new(&[op::READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}

pub fn read_capacity_16() -> Cdb {
    let mut cdb = [0u8; 16];
    cdb[0] = op::SERVICE_ACTION_IN_16;
    cdb[1] = op::READ_CAPACITY_16;
    cdb[13] = CAPACITY_16_LEN;
    Cdb::new(&cdb)
}

/// MODE SENSE(6) of every page, no block descriptors: only the header's
/// write-protect bit is used.
pub fn mode_sense_6() -> Cdb {
    Cdb::new(&[op::MODE_SENSE_6, 0x08, 0x3F, 0, MODE_SENSE_LEN, 0])
}

/// SYNCHRONIZE CACHE(10) of the whole medium (LBA 0, 0 blocks).
pub fn synchronize_cache() -> Cdb {
    Cdb::new(&[op::SYNCHRONIZE_CACHE_10, 0, 0, 0, 0, 0, 0, 0, 0, 0])
}

/// READ or WRITE of `blocks` blocks at `lba`: the 10-byte form when the
/// range fits it (LBA below 2^32, at most 65535 blocks), else the 16-byte
/// form (devices over 2 TiB). `None` for zero blocks.
pub fn rw(write: bool, lba: u64, blocks: u32) -> Option<Cdb> {
    if blocks == 0 {
        return None;
    }
    let ten = u32::try_from(lba).ok().zip(u16::try_from(blocks).ok());
    Some(match ten {
        Some((lba, blocks)) => {
            let opcode = if write { op::WRITE_10 } else { op::READ_10 };
            let mut cdb = [0u8; 10];
            cdb[0] = opcode;
            cdb[2..6].copy_from_slice(&lba.to_be_bytes());
            cdb[7..9].copy_from_slice(&blocks.to_be_bytes());
            Cdb::new(&cdb)
        }
        None => {
            let opcode = if write { op::WRITE_16 } else { op::READ_16 };
            let mut cdb = [0u8; 16];
            cdb[0] = opcode;
            cdb[2..10].copy_from_slice(&lba.to_be_bytes());
            cdb[10..14].copy_from_slice(&blocks.to_be_bytes());
            Cdb::new(&cdb)
        }
    })
}

/// Peripheral device type of a direct-access block device (SBC).
pub const DIRECT_ACCESS: u8 = 0x00;

/// What INQUIRY says about the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inquiry {
    /// Peripheral qualifier (0: a device is connected to this LUN).
    pub qualifier: u8,
    pub device_type: u8,
    pub removable: bool,
    /// T10 vendor identification and product identification, printable
    /// ASCII (anything else becomes `?`), space padded.
    pub vendor: [u8; 8],
    pub product: [u8; 16],
}

/// Parse standard INQUIRY data. At least the first five bytes must be there;
/// the identification strings are read as far as the device sent them.
pub fn parse_inquiry(data: &[u8]) -> Result<Inquiry, Error> {
    if data.len() < 5 {
        return Err(Error::Short);
    }
    let mut inquiry = Inquiry {
        qualifier: data[0] >> 5,
        device_type: data[0] & 0x1F,
        removable: data[1] & 0x80 != 0,
        vendor: [b' '; 8],
        product: [b' '; 16],
    };
    copy_ascii(&mut inquiry.vendor, data.get(8..16).unwrap_or(&[]));
    copy_ascii(&mut inquiry.product, data.get(16..32).unwrap_or(&[]));
    Ok(inquiry)
}

fn copy_ascii(out: &mut [u8], from: &[u8]) {
    for (to, &byte) in out.iter_mut().zip(from) {
        *to = if (0x20..0x7F).contains(&byte) {
            byte
        } else {
            b'?'
        };
    }
}

/// Sense keys this driver acts on.
pub mod key {
    pub const NO_SENSE: u8 = 0x0;
    pub const RECOVERED_ERROR: u8 = 0x1;
    pub const NOT_READY: u8 = 0x2;
    pub const MEDIUM_ERROR: u8 = 0x3;
    pub const ILLEGAL_REQUEST: u8 = 0x5;
    pub const UNIT_ATTENTION: u8 = 0x6;
    pub const DATA_PROTECT: u8 = 0x7;
    pub const ABORTED_COMMAND: u8 = 0xB;
}

/// Additional sense code: MEDIUM NOT PRESENT.
pub const ASC_MEDIUM_NOT_PRESENT: u8 = 0x3A;

/// Sense data reduced to what decides the next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
}

/// Parse fixed-format (0x70/0x71) or descriptor-format (0x72/0x73) sense
/// data. Fixed data shorter than 14 bytes keeps the key and reports no
/// additional sense code.
pub fn parse_sense(data: &[u8]) -> Result<Sense, Error> {
    let code = data.first().ok_or(Error::Short)? & 0x7F;
    match code {
        0x70 | 0x71 => {
            let key = data.get(2).ok_or(Error::Short)? & 0x0F;
            let asc = data.get(12).copied().unwrap_or(0);
            let ascq = data.get(13).copied().unwrap_or(0);
            Ok(Sense { key, asc, ascq })
        }
        0x72 | 0x73 => {
            if data.len() < 4 {
                return Err(Error::Short);
            }
            Ok(Sense {
                key: data[1] & 0x0F,
                asc: data[2],
                ascq: data[3],
            })
        }
        _ => Err(Error::Malformed),
    }
}

/// What a failed command's sense data means for the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Try the command again at once (UNIT ATTENTION after a reset or a media
    /// change, an aborted command, no sense at all).
    Retry,
    /// The unit is becoming ready: wait, then retry.
    Wait,
    /// No medium in the drive.
    NoMedium,
    /// The medium is write protected.
    WriteProtected,
    /// The device does not support the command (ILLEGAL REQUEST).
    Unsupported,
    /// A real failure (medium or hardware error).
    Fail,
}

impl Sense {
    pub fn action(&self) -> Action {
        match self.key {
            key::UNIT_ATTENTION | key::ABORTED_COMMAND | key::NO_SENSE | key::RECOVERED_ERROR => {
                Action::Retry
            }
            key::NOT_READY if self.asc == ASC_MEDIUM_NOT_PRESENT => Action::NoMedium,
            key::NOT_READY => Action::Wait,
            key::DATA_PROTECT => Action::WriteProtected,
            key::ILLEGAL_REQUEST => Action::Unsupported,
            _ => Action::Fail,
        }
    }
}

/// The medium's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    /// Number of logical blocks (last LBA + 1).
    pub blocks: u64,
    /// Bytes per logical block: 512, 1024, 2048 or 4096.
    pub block_len: u32,
}

/// READ CAPACITY(10). `Ok(None)` when the device says the medium is too
/// large for it (last LBA `0xFFFFFFFF`): ask READ CAPACITY(16).
pub fn parse_capacity_10(data: &[u8]) -> Result<Option<Capacity>, Error> {
    if data.len() < CAPACITY_10_LEN {
        return Err(Error::Short);
    }
    let last = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if last == u32::MAX {
        return Ok(None);
    }
    let block_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    capacity(u64::from(last), block_len).map(Some)
}

/// READ CAPACITY(16): the first 12 bytes carry the last LBA and block length.
pub fn parse_capacity_16(data: &[u8]) -> Result<Capacity, Error> {
    if data.len() < 12 {
        return Err(Error::Short);
    }
    let mut last = [0u8; 8];
    last.copy_from_slice(&data[0..8]);
    let block_len = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    capacity(u64::from_be_bytes(last), block_len)
}

fn capacity(last: u64, block_len: u32) -> Result<Capacity, Error> {
    if !matches!(block_len, 512 | 1024 | 2048 | 4096) {
        return Err(Error::Malformed);
    }
    let blocks = last.checked_add(1).ok_or(Error::BadLength)?;
    // A medium larger than 2^64 bytes cannot be addressed; refuse it.
    blocks
        .checked_mul(u64::from(block_len))
        .ok_or(Error::BadLength)?;
    Ok(Capacity { blocks, block_len })
}

/// The write-protect bit of a MODE SENSE(6) header (SBC-3 6.4.2: the
/// device-specific parameter's bit 7).
pub fn parse_write_protect(data: &[u8]) -> Result<bool, Error> {
    if data.len() < 4 {
        return Err(Error::Short);
    }
    Ok(data[2] & 0x80 != 0)
}
