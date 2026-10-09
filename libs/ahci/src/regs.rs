//! HBA and port registers (AHCI 1.3.1 sections 3.1 and 3.3). Offsets are
//! from the start of the HBA's memory (ABAR); port `n`'s registers start at
//! [`port_base`]. Every bit used here is named after the specification.

/// Host Capabilities.
pub const CAP: usize = 0x00;
/// Global HBA Control.
pub const GHC: usize = 0x04;
/// Interrupt Status (one bit per port).
pub const IS: usize = 0x08;
/// Ports Implemented.
pub const PI: usize = 0x0C;
/// Version (`major << 16 | minor`, BCD-ish).
pub const VS: usize = 0x10;
/// Host Capabilities Extended.
pub const CAP2: usize = 0x24;
/// BIOS/OS Handoff Control and Status.
pub const BOHC: usize = 0x28;

/// Offset of port 0's registers.
pub const PORT_BASE: usize = 0x100;
/// Bytes of registers per port.
pub const PORT_STRIDE: usize = 0x80;
/// Most ports one HBA has.
pub const MAX_PORTS: usize = 32;

/// First register of port `index`.
pub const fn port_base(index: usize) -> usize {
    PORT_BASE + index * PORT_STRIDE
}

/// Bytes of ABAR that cover the registers of ports `0..=highest`.
pub const fn bar_bytes(highest: usize) -> u64 {
    (PORT_BASE + (highest + 1) * PORT_STRIDE) as u64
}

pub mod cap {
    pub const NP_MASK: u32 = 0x1F;
    pub const NCS_SHIFT: u32 = 8;
    pub const SSS: u32 = 1 << 27;
    pub const S64A: u32 = 1 << 31;
}

pub mod cap2 {
    /// BIOS/OS handoff is implemented.
    pub const BOH: u32 = 1;
}

pub mod ghc {
    pub const HR: u32 = 1;
    pub const IE: u32 = 1 << 1;
    pub const AE: u32 = 1 << 31;
}

pub mod bohc {
    /// BIOS Owned Semaphore.
    pub const BOS: u32 = 1;
    /// OS Owned Semaphore.
    pub const OOS: u32 = 1 << 1;
    /// BIOS Busy.
    pub const BB: u32 = 1 << 4;
}

/// Port register offsets, relative to [`port_base`].
pub mod px {
    pub const CLB: usize = 0x00;
    pub const CLBU: usize = 0x04;
    pub const FB: usize = 0x08;
    pub const FBU: usize = 0x0C;
    pub const IS: usize = 0x10;
    pub const IE: usize = 0x14;
    pub const CMD: usize = 0x18;
    pub const TFD: usize = 0x20;
    pub const SIG: usize = 0x24;
    pub const SSTS: usize = 0x28;
    pub const SCTL: usize = 0x2C;
    pub const SERR: usize = 0x30;
    pub const SACT: usize = 0x34;
    pub const CI: usize = 0x38;
}

pub mod cmd {
    /// Start.
    pub const ST: u32 = 1;
    /// FIS Receive Enable.
    pub const FRE: u32 = 1 << 4;
    /// FIS Receive Running.
    pub const FR: u32 = 1 << 14;
    /// Command List Running.
    pub const CR: u32 = 1 << 15;
}

pub mod is {
    /// Task File Error Status.
    pub const TFES: u32 = 1 << 30;
    /// Host Bus Fatal Error.
    pub const HBFS: u32 = 1 << 29;
    /// Host Bus Data Error.
    pub const HBDS: u32 = 1 << 28;
    /// Interface Fatal Error.
    pub const IFS: u32 = 1 << 27;
    /// Anything that stops a port's command processing or marks the
    /// transfer bad.
    pub const FATAL: u32 = TFES | HBFS | HBDS | IFS;
}

pub mod tfd {
    pub const ERR: u32 = 1;
    pub const DRQ: u32 = 1 << 3;
    pub const BSY: u32 = 1 << 7;
    pub const BUSY_MASK: u32 = BSY | DRQ;
}

pub mod ssts {
    pub const DET_MASK: u32 = 0xF;
    /// Device present and Phy communication established.
    pub const DET_PRESENT: u32 = 3;
    pub const IPM_SHIFT: u32 = 8;
    pub const IPM_MASK: u32 = 0xF;
    /// Interface in active state.
    pub const IPM_ACTIVE: u32 = 1;
}

pub mod sctl {
    pub const DET_MASK: u32 = 0xF;
    /// Perform interface initialisation (COMRESET).
    pub const DET_RESET: u32 = 1;
}

/// `PxSIG` of an ATA disk (SATA signature, sector count 1, LBA 1).
pub const SIG_ATA: u32 = 0x0000_0101;
/// `PxSIG` of an ATAPI device.
pub const SIG_ATAPI: u32 = 0xEB14_0101;

/// `CAP` decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    /// Number of ports (`CAP.NP` + 1).
    pub ports: u8,
    /// Command slots per port (`CAP.NCS` + 1).
    pub slots: u8,
    /// 64-bit DMA addresses are supported.
    pub s64a: bool,
}

impl Cap {
    pub fn decode(raw: u32) -> Cap {
        Cap {
            ports: (raw & cap::NP_MASK) as u8 + 1,
            slots: ((raw >> cap::NCS_SHIFT) & 0x1F) as u8 + 1,
            s64a: raw & cap::S64A != 0,
        }
    }
}

/// `(major, minor)` of a `VS` value (`0x0001_0301` is 1.3.1: minor 3,
/// patch 1; the patch digit is dropped).
pub fn version(raw: u32) -> (u16, u16) {
    ((raw >> 16) as u16, (raw as u16) >> 8)
}
