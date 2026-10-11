//! RTL8822C constants and tables read by the MAC and PCIe code (upstream
//! `rtw8822c_hw_spec` and the tables it points at in `rtw8822c.c`).
//!
//! Owner: the core/MAC work package.

/// Bytes of TX packet descriptor.
pub const TX_PKT_DESC_SZ: usize = 48;
/// Bytes of TX buffer descriptor (PCIe).
pub const TX_BUF_DESC_SZ: usize = 16;
/// Bytes of RX packet descriptor.
pub const RX_PKT_DESC_SZ: usize = 24;
/// Bytes of RX buffer descriptor (PCIe).
pub const RX_BUF_DESC_SZ: usize = 8;
pub const PHY_EFUSE_SIZE: u32 = 512;
pub const LOG_EFUSE_SIZE: u32 = 768;
pub const PTCT_EFUSE_SIZE: u32 = 124;
pub const TXFF_SIZE: u32 = 262_144;
pub const RXFF_SIZE: u32 = 24_576;
pub const FW_RXFF_SIZE: u32 = 12_288;
pub const RSVD_DRV_PG_NUM: u32 = 16;
pub const CSI_BUF_PG_NUM: u32 = 50;
pub const SYS_FUNC_EN: u8 = 0xD8;
pub const RF_BASE_ADDR: [u32; 2] = [0x3c00, 0x4c00];
pub const RF_SIPI_ADDR: [u32; 2] = [0x1808, 0x4108];
pub const FW_FIFO_ADDR: [u32; 6] = [0x780, 0x700, 0x780, 0x660, 0x650, 0x680];
