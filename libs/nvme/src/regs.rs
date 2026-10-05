//! Controller registers (NVMe 1.4 section 3.1), as offsets into BAR0.
//!
//! 64-bit registers are accessed as two 32-bit halves, low first, which the
//! specification allows for every register and which works on any BAR
//! mapping.

use crate::Platform;

/// Controller Capabilities (64-bit).
pub const CAP: usize = 0x00;
/// Version.
pub const VS: usize = 0x08;
/// Interrupt Mask Set / Clear.
pub const INTMS: usize = 0x0C;
pub const INTMC: usize = 0x10;
/// Controller Configuration.
pub const CC: usize = 0x14;
/// Controller Status.
pub const CSTS: usize = 0x1C;
/// Admin Queue Attributes.
pub const AQA: usize = 0x24;
/// Admin Submission / Completion Queue Base Address (64-bit).
pub const ASQ: usize = 0x28;
pub const ACQ: usize = 0x30;
/// First doorbell register.
pub const DOORBELLS: usize = 0x1000;

/// `CC` fields.
pub mod cc {
    pub const EN: u32 = 1 << 0;
    /// I/O command set: NVM (bits 6:4 = 0).
    pub const CSS_NVM: u32 = 0 << 4;
    /// Memory page size 2^(12 + MPS): 4 KiB.
    pub const MPS_4K: u32 = 0 << 7;
    /// Round-robin arbitration.
    pub const AMS_RR: u32 = 0 << 11;
    pub const SHN_MASK: u32 = 3 << 14;
    /// Normal shutdown notification.
    pub const SHN_NORMAL: u32 = 1 << 14;
    /// Submission entry size 2^6 = 64 bytes.
    pub const IOSQES_64: u32 = 6 << 16;
    /// Completion entry size 2^4 = 16 bytes.
    pub const IOCQES_16: u32 = 4 << 20;
}

/// `CSTS` fields.
pub mod csts {
    pub const RDY: u32 = 1 << 0;
    pub const CFS: u32 = 1 << 1;
    pub const SHST_MASK: u32 = 3 << 2;
    /// Shutdown processing complete.
    pub const SHST_COMPLETE: u32 = 2 << 2;
}

/// Decoded `CAP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    /// Maximum queue entries supported, as a count (the register is 0-based).
    pub mqes: u32,
    /// Queues must be physically contiguous.
    pub cqr: bool,
    /// Worst-case time for `CSTS.RDY` to follow `CC.EN`, in milliseconds.
    pub timeout_ms: u64,
    /// Bytes between two doorbell registers (`4 << DSTRD`).
    pub doorbell_stride: usize,
    /// The NVM command set is supported (`CSS` bit 0).
    pub nvm: bool,
    /// Smallest and largest memory page size, as powers of two.
    pub page_min_shift: u32,
    pub page_max_shift: u32,
}

impl Cap {
    pub fn decode(raw: u64) -> Cap {
        let to = (raw >> 24) & 0xFF;
        Cap {
            mqes: (raw & 0xFFFF) as u32 + 1,
            cqr: raw & (1 << 16) != 0,
            // A zero timeout is a broken register: give it one unit.
            timeout_ms: to.max(1) * 500,
            doorbell_stride: 4 << ((raw >> 32) & 0xF),
            nvm: raw & (1 << 37) != 0,
            page_min_shift: 12 + ((raw >> 48) & 0xF) as u32,
            page_max_shift: 12 + ((raw >> 52) & 0xF) as u32,
        }
    }
}

/// Read a 64-bit register as two halves, low first.
pub fn read64(platform: &dyn Platform, offset: usize) -> u64 {
    let low = platform.read32(offset);
    let high = platform.read32(offset + 4);
    u64::from(high) << 32 | u64::from(low)
}

/// Write a 64-bit register as two halves, low first.
pub fn write64(platform: &dyn Platform, offset: usize, value: u64) {
    platform.write32(offset, value as u32);
    platform.write32(offset + 4, (value >> 32) as u32);
}

/// Offset of queue `qid`'s submission tail doorbell.
pub fn sq_doorbell(qid: u16, stride: usize) -> usize {
    DOORBELLS + 2 * usize::from(qid) * stride
}

/// Offset of queue `qid`'s completion head doorbell.
pub fn cq_doorbell(qid: u16, stride: usize) -> usize {
    DOORBELLS + (2 * usize::from(qid) + 1) * stride
}

/// `AQA` for admin queues of `sq` and `cq` entries.
pub fn aqa(sq: u16, cq: u16) -> u32 {
    (u32::from(cq) - 1) << 16 | (u32::from(sq) - 1)
}

/// The `CC` value that enables the controller with this driver's settings.
pub fn cc_enable() -> u32 {
    cc::EN | cc::CSS_NVM | cc::MPS_4K | cc::AMS_RR | cc::IOSQES_64 | cc::IOCQES_16
}

/// The version register as `(major, minor)`.
pub fn version(raw: u32) -> (u16, u8) {
    ((raw >> 16) as u16, (raw >> 8) as u8)
}
