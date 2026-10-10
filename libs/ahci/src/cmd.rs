//! Command list entries, command tables and PRDT planning (AHCI 1.3.1
//! sections 4.2.2 and 4.2.3).
//!
//! A command is a *slot*: a 32-byte header in the port's command list, whose
//! `CTBA` points at the slot's command table (the command FIS at offset 0,
//! the PRDT at [`PRDT_OFFSET`]), plus the slot's bit in `PxCI`.

use crate::fis::H2D_BYTES;

/// Bytes in one command header.
pub const HEADER_BYTES: usize = 32;
/// Bytes in one PRDT entry.
pub const PRD_BYTES: usize = 16;
/// Offset of the PRDT in a command table.
pub const PRDT_OFFSET: usize = 0x80;
/// PRDT entries per command this driver builds.
pub const MAX_PRD: usize = 64;
/// Bytes of one command table with a full PRDT.
pub const TABLE_BYTES: usize = PRDT_OFFSET + MAX_PRD * PRD_BYTES;
/// Command slots the driver uses per port.
pub const MAX_SLOTS: usize = 8;
/// Bytes of the command list (32 slots); the list is 1 KiB aligned.
pub const COMMAND_LIST_BYTES: usize = 32 * HEADER_BYTES;
/// Most data bytes in one PRDT entry (22-bit count, bit 0 is "odd" so the
/// count is even).
pub const MAX_PRD_BYTES: usize = 4 << 20;
/// Bytes in one command at most: 512 sectors (the library's choice, below
/// the 65536-sector limit of the 16-bit count).
pub const MAX_COMMAND_BYTES: usize = 256 * 1024;

/// Header bit: the command writes to the device.
const WRITE: u32 = 1 << 6;
/// Header: the FIS is five dwords.
const CFL: u32 = (H2D_BYTES / 4) as u32;

/// A command header in the command list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub write: bool,
    pub prdtl: u16,
    /// Physical address of the command table (128-byte aligned).
    pub ctba: u64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut raw = [0u8; HEADER_BYTES];
        let dw0 = CFL | if self.write { WRITE } else { 0 } | u32::from(self.prdtl) << 16;
        raw[0..4].copy_from_slice(&dw0.to_le_bytes());
        // dw1 (PRDBC) starts at zero: the HBA counts into it.
        raw[8..12].copy_from_slice(&(self.ctba as u32).to_le_bytes());
        raw[12..16].copy_from_slice(&((self.ctba >> 32) as u32).to_le_bytes());
        raw
    }

    pub fn decode(raw: &[u8; HEADER_BYTES]) -> Header {
        let dw0 = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let lo = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
        let hi = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
        Header {
            write: dw0 & WRITE != 0,
            prdtl: (dw0 >> 16) as u16,
            ctba: u64::from(hi) << 32 | u64::from(lo),
        }
    }
}

/// Where the HBA reports the bytes it moved (header dword 1).
pub const PRDBC_OFFSET: usize = 4;

/// One physical region of a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prd {
    pub addr: u64,
    /// Bytes (even, 2..=[`MAX_PRD_BYTES`]).
    pub bytes: u32,
}

impl Prd {
    pub fn encode(&self) -> [u8; PRD_BYTES] {
        let mut raw = [0u8; PRD_BYTES];
        raw[0..8].copy_from_slice(&self.addr.to_le_bytes());
        // dw3: byte count minus one (bit 0 set: an even count), no interrupt.
        raw[12..16].copy_from_slice(&(self.bytes - 1).to_le_bytes());
        raw
    }

    pub fn decode(raw: &[u8; PRD_BYTES]) -> Prd {
        let dw3 = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
        Prd {
            addr: u64::from_le_bytes(raw[0..8].try_into().expect("8 bytes")),
            bytes: (dw3 & 0x3F_FFFF) + 1,
        }
    }
}

/// Where a plan stands in the caller's segments.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    /// Index of the current segment.
    pub segment: usize,
    /// Bytes of it already planned.
    pub offset: usize,
}

impl Cursor {
    /// Move forward `bytes` bytes through `segments`.
    pub fn advance(&mut self, segments: &[(u64, usize)], mut bytes: usize) {
        while self.segment < segments.len() {
            let left = segments[self.segment].1 - self.offset;
            if bytes < left {
                self.offset += bytes;
                return;
            }
            bytes -= left;
            self.segment += 1;
            self.offset = 0;
        }
    }
}

/// Why a command could not be planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// A page has no physical address.
    Unmapped,
    /// The buffer breaks an AHCI rule (odd address or length, an address
    /// above 4 GiB on an HBA that cannot address it, fewer than one sector
    /// in the PRDT's entries): the caller bounces it.
    Misaligned,
}

/// One command's data: its PRDT and byte count.
#[derive(Clone, Debug)]
pub struct Plan {
    pub prds: [Prd; MAX_PRD],
    pub count: usize,
    pub bytes: usize,
}

impl Plan {
    pub fn entries(&self) -> &[Prd] {
        &self.prds[..self.count]
    }
}

/// Cut the next command (at most `max` bytes, a multiple of `sector`) out of
/// `segments` from `cursor`. Entries never cross a page; an entry's address
/// and length must be even. Without `s64a` every entry must lie below 4 GiB.
pub fn plan(
    segments: &[(u64, usize)],
    cursor: Cursor,
    max: usize,
    sector: usize,
    s64a: bool,
    translate: &dyn Fn(u64) -> Option<u64>,
) -> Result<Plan, PlanError> {
    let mut plan = Plan {
        prds: [Prd { addr: 0, bytes: 2 }; MAX_PRD],
        count: 0,
        bytes: 0,
    };
    let mut at = cursor;
    while at.segment < segments.len() && plan.bytes < max && plan.count < MAX_PRD {
        let (base, len) = segments[at.segment];
        let virt = base + at.offset as u64;
        let in_page = 4096 - (virt % 4096) as usize;
        let take = (len - at.offset).min(in_page).min(max - plan.bytes);
        if take == 0 {
            at.segment += 1;
            at.offset = 0;
            continue;
        }
        let phys = translate(virt).ok_or(PlanError::Unmapped)?;
        if !phys.is_multiple_of(2) || !take.is_multiple_of(2) {
            return Err(PlanError::Misaligned);
        }
        if !s64a && phys.saturating_add(take as u64) > 1 << 32 {
            return Err(PlanError::Misaligned);
        }
        // Merge with the previous entry when physically adjacent.
        match plan.prds[..plan.count].last_mut() {
            Some(last)
                if last.addr + u64::from(last.bytes) == phys
                    && last.bytes as usize + take <= MAX_PRD_BYTES =>
            {
                last.bytes += take as u32;
            }
            _ => {
                plan.prds[plan.count] = Prd {
                    addr: phys,
                    bytes: take as u32,
                };
                plan.count += 1;
            }
        }
        plan.bytes += take;
        at.advance(segments, take);
    }
    // Whole sectors only: trim the tail back to a sector boundary.
    let whole = plan.bytes - plan.bytes % sector;
    if whole == 0 {
        return Err(PlanError::Misaligned);
    }
    let mut excess = plan.bytes - whole;
    while excess > 0 {
        let last = &mut plan.prds[plan.count - 1];
        let cut = excess.min(last.bytes as usize);
        last.bytes -= cut as u32;
        excess -= cut;
        if last.bytes == 0 {
            plan.count -= 1;
        }
    }
    plan.bytes = whole;
    // A trim can leave an odd entry only if sectors are odd; they are not.
    Ok(plan)
}
