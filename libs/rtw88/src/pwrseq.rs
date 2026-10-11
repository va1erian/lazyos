//! Power sequence commands (upstream `struct rtw_pwr_seq_cmd`, `main.h`).
//!
//! The chip powers its MAC on and off by running a table of register steps;
//! the tables are chip data ([`crate::rtw8822c::pwrseq`]) and the engine that
//! runs them is [`crate::mac::pwr_seq_parser`].

/// What a step does (upstream `RTW_PWR_CMD_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PwrCmd {
    /// Read-modify-write `value` under `mask`.
    Write = 0x01,
    /// Poll until `(read & mask) == (value & mask)`.
    Polling = 0x02,
    /// Wait `offset` microseconds (`value` 0) or milliseconds (`value` 1).
    Delay = 0x03,
    /// End of the table.
    End = 0x04,
}

/// The register space a step addresses (upstream `RTW_PWR_ADDR_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PwrBase {
    Mac = 0x00,
    Usb = 0x01,
    Pcie = 0x02,
    Sdio = 0x03,
}

/// Interface mask bits (upstream `RTW_PWR_INTF_*_MSK`).
pub mod intf {
    pub const SDIO: u8 = 1 << 0;
    pub const USB: u8 = 1 << 1;
    pub const PCI: u8 = 1 << 2;
    pub const ALL: u8 = SDIO | USB | PCI;
}

/// Cut (chip revision) mask bits (upstream `RTW_PWR_CUT_*_MSK`).
pub mod cut {
    pub const TEST: u8 = 1 << 0;
    pub const A: u8 = 1 << 1;
    pub const B: u8 = 1 << 2;
    pub const C: u8 = 1 << 3;
    pub const D: u8 = 1 << 4;
    pub const E: u8 = 1 << 5;
    pub const F: u8 = 1 << 6;
    pub const G: u8 = 1 << 7;
    pub const ALL: u8 = 0xFF;
}

/// One step of a power sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwrSeqCmd {
    pub offset: u16,
    pub cut_mask: u8,
    pub intf_mask: u8,
    pub base: PwrBase,
    pub cmd: PwrCmd,
    pub mask: u8,
    pub value: u8,
}
