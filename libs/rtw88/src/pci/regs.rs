//! PCIe register map of the 8822CE (upstream `pci.h`, plus the few `reg.h`
//! registers the PCIe code touches). Offsets are into BAR 2.

/// DMA control: `RTK_PCI_CTRL`.
pub const PCI_CTRL: u32 = 0x300;
pub const BIT_RST_TRXDMA_INTF: u32 = 1 << 20;
pub const BIT_RX_TAG_EN: u32 = 1 << 15;

/// DBI (the PCIe core's own configuration space, reached through the MAC).
pub const REG_DBI_WDATA_V1: u32 = 0x03E8;
pub const REG_DBI_RDATA_V1: u32 = 0x03EC;
pub const REG_DBI_FLAG_V1: u32 = 0x03F0;
pub const BIT_DBI_RFLAG: u32 = 1 << 17;
pub const BIT_DBI_WFLAG: u32 = 1 << 16;
/// `GENMASK(15, 12)`: the byte-enable nibble of a DBI write.
pub const BITS_DBI_WREN: u16 = 0xF000;
/// `GENMASK(11, 2)`: the dword address of a DBI access.
pub const BITS_DBI_ADDR_MASK: u16 = 0x0FFC;

/// MDIO (the PCIe PHY).
pub const REG_MDIO_V1: u32 = 0x03F4;
pub const REG_PCIE_MIX_CFG: u32 = 0x03F8;
pub const BITS_MDIO_ADDR_MASK: u8 = 0x1F;
pub const BIT_MDIO_WFLAG_V1: u32 = 1 << 5;
pub const RTW_PCI_MDIO_PG_SZ: u8 = 1 << 5;
pub const RTW_PCI_MDIO_PG_OFFS_G1: u8 = 0;
pub const RTW_PCI_MDIO_PG_OFFS_G2: u8 = 2;
/// Polls of a DBI/MDIO flag before giving up (10 us apart).
pub const RTW_PCI_WR_RETRY_CNT: u32 = 20;

/// DBI addresses of the chip-side link power controls.
pub const RTK_PCIE_LINK_CFG: u16 = 0x0719;
pub const BIT_CLKREQ_SW_EN: u8 = 1 << 4;
pub const BIT_L1_SW_EN: u8 = 1 << 3;
pub const BIT_CLKREQ_N_PAD: u8 = 1 << 0;
pub const RTK_PCIE_CLKDLY_CTRL: u16 = 0x0725;

/// Buffer-descriptor ring base addresses (`*_DESA_*`).
pub const TXBD_DESA_BCNQ: u32 = 0x308;
pub const TXBD_DESA_H2CQ: u32 = 0x1320;
pub const TXBD_DESA_MGMTQ: u32 = 0x310;
pub const TXBD_DESA_BKQ: u32 = 0x330;
pub const TXBD_DESA_BEQ: u32 = 0x328;
pub const TXBD_DESA_VIQ: u32 = 0x320;
pub const TXBD_DESA_VOQ: u32 = 0x318;
pub const TXBD_DESA_HI0Q: u32 = 0x340;
pub const RXBD_DESA_MPDUQ: u32 = 0x338;

/// Host index (bits 11:0, written) and hardware index (bits 27:16, read).
pub const TRX_BD_IDX_MASK: u32 = 0xFFF;
pub const TRX_BD_HW_IDX_SHIFT: u32 = 16;

/// Ring sizes (`*_NUM_*`, 16-bit). The beacon queue has none: one entry.
pub const TXBD_NUM_H2CQ: u32 = 0x1328;
pub const TXBD_NUM_MGMTQ: u32 = 0x380;
pub const TXBD_NUM_BKQ: u32 = 0x38A;
pub const TXBD_NUM_BEQ: u32 = 0x388;
pub const TXBD_NUM_VIQ: u32 = 0x386;
pub const TXBD_NUM_VOQ: u32 = 0x384;
pub const TXBD_NUM_HI0Q: u32 = 0x38C;
pub const RXBD_NUM_MPDUQ: u32 = 0x382;

/// Ring indices (`*_IDX_*`).
pub const TXBD_IDX_H2CQ: u32 = 0x132C;
pub const TXBD_IDX_MGMTQ: u32 = 0x3B0;
pub const TXBD_IDX_BKQ: u32 = 0x3AC;
pub const TXBD_IDX_BEQ: u32 = 0x3A8;
pub const TXBD_IDX_VIQ: u32 = 0x3A4;
pub const TXBD_IDX_VOQ: u32 = 0x3A0;
pub const TXBD_IDX_HI0Q: u32 = 0x3B8;
pub const RXBD_IDX_MPDUQ: u32 = 0x3B4;

pub const TXBD_RWPTR_CLR: u32 = 0x39C;
pub const TXBD_H2CQ_CSR: u32 = 0x1330;
pub const BIT_CLR_H2CQ_HOST_IDX: u32 = 1 << 16;
pub const BIT_CLR_H2CQ_HW_IDX: u32 = 1 << 8;

/// The beacon queue's "go" register and flag (`RTK_PCI_TXBD_BCN_WORK`).
pub const TXBD_BCN_WORK: u32 = 0x383;
pub const BIT_PCI_BCNQ_FLAG: u8 = 1 << 4;
/// The OWN bit in a beacon buffer descriptor's `psb_len`.
pub const TXBD_OWN_OFFSET: u32 = 15;

/// Interrupt mask and status registers.
pub const HIMR0: u32 = 0x0B0;
pub const HISR0: u32 = 0x0B4;
pub const HIMR1: u32 = 0x0B8;
pub const HISR1: u32 = 0x0BC;
pub const HIMR2: u32 = 0x10B0;
pub const HISR2: u32 = 0x10B4;
pub const HIMR3: u32 = 0x10B8;
pub const HISR3: u32 = 0x10BC;

/// IMR/ISR 0 bits.
pub const IMR_TBDER: u32 = 1 << 26;
pub const IMR_TBDOK: u32 = 1 << 25;
pub const IMR_BCNDMAINT0: u32 = 1 << 20;
pub const IMR_BCNDOK0: u32 = 1 << 16;
pub const IMR_BCNDMAINT_E: u32 = 1 << 14;
pub const IMR_C2HCMD: u32 = 1 << 10;
pub const IMR_HIGHDOK: u32 = 1 << 7;
pub const IMR_MGNTDOK: u32 = 1 << 6;
pub const IMR_BKDOK: u32 = 1 << 5;
pub const IMR_BEDOK: u32 = 1 << 4;
pub const IMR_VIDOK: u32 = 1 << 3;
pub const IMR_VODOK: u32 = 1 << 2;
pub const IMR_RDU: u32 = 1 << 1;
pub const IMR_ROK: u32 = 1 << 0;
/// IMR/ISR 1 bits.
pub const IMR_TXFIFO_TH_INT: u32 = 1 << 30;
pub const IMR_MCUERR: u32 = 1 << 28;
pub const IMR_TXERR: u32 = 1 << 11;
pub const IMR_RXERR: u32 = 1 << 10;
pub const IMR_TXFOVW: u32 = 1 << 9;
pub const IMR_RXFOVW: u32 = 1 << 8;
/// IMR/ISR 3 bits.
pub const IMR_H2CDOK: u32 = 1 << 16;

/// Registers outside `pci.h` that the PCIe code writes (`reg.h`).
pub const REG_HCI_MIX_CFG: u32 = 0x03FC;
pub const BIT_PCIE_EMAC_PDN_AUX_TO_FAST_CLK: u32 = 1 << 26;
pub const REG_ANAPARSW_MAC_0: u32 = 0x1010;
/// `GENMASK(29, 28)`.
pub const BIT_CF_L_V2: u32 = 0x3 << 28;
/// The reserved-page head and its "beacon valid" flag (written 1 to clear,
/// set by the hardware once the beacon queue's page landed).
pub const REG_FIFOPAGE_CTRL_2: u32 = 0x0204;
pub const BIT_BCN_VALID_V1: u16 = 1 << 15;

/// `RTW_CHIP_VER_CUT_D`.
pub const CHIP_VER_CUT_D: u8 = 0x03;
