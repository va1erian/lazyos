//! RTL8822C constants and tables read by the MAC and PCIe code (upstream
//! `rtw8822c_hw_spec` and the tables it points at in `rtw8822c.c`).
//!
//! Owner: the core/MAC work package. Only the `RTW_CHIP_TYPE_8822C`,
//! `RTW_WCPU_3081`, PCIe values are kept: where upstream indexes a table by
//! bus type, the PCIe row is the one the MAC code picks.

use crate::pwrseq::{cut, intf, PwrBase, PwrCmd, PwrSeqCmd};

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

/// TX FIFO page size (upstream `TX_PAGE_SIZE`, `chip->page_size`).
pub const TX_PAGE_SIZE_SHIFT: u32 = 7;
pub const TX_PAGE_SIZE: u32 = 1 << TX_PAGE_SIZE_SHIFT;

/// Bytes of a firmware C2H report of a rate change (`c2h_ra_report_size`).
pub const C2H_RA_REPORT_SIZE: usize = 7;
/// The chip reports its hardware features through `REG_C2HEVT` after the
/// firmware download (`hw_feature_report`).
pub const HW_FEATURE_REPORT: bool = true;
/// Number of RFE definitions (`rtw8822c_rfe_defs`); every one of them has a
/// power-by-rate and a power-limit table, so an `rfe_option` below this is
/// supported (`rtw_check_supported_rfe`).
pub const RFE_DEFS_SIZE: u8 = 7;
/// Hardware security CAM entries (`rtwdev->sec.total_cam_num`).
pub const TOTAL_CAM_NUM: u8 = 32;
/// The PCIe RPWM/CPWM registers (`rtw_chip_parameter_setup`, PCIe case).
pub const RPWM_ADDR: u32 = 0x03d9;
pub const CPWM_ADDR: u32 = 0x03da;
/// LTE coexistence indirect access registers (`rtw8822c_ltecoex_addr`).
pub const LTECOEX_CTRL: u32 = 0x1700;
pub const LTECOEX_WDATA: u32 = 0x1704;
pub const LTECOEX_RDATA: u32 = 0x1708;

/// How a TX priority queue maps to a DMA channel (upstream
/// `enum rtw_dma_mapping`, values are the hardware's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DmaMapping {
    Extra = 0,
    Low = 1,
    Normal = 2,
    High = 3,
}

/// Number of DMA mappings (`RTW_DMA_MAPPING_MAX`).
pub const DMA_MAPPING_MAX: usize = 4;

/// Reserved TX FIFO pages per DMA channel (upstream `struct rtw_page_table`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTable {
    pub hq_num: u16,
    pub nq_num: u16,
    pub lq_num: u16,
    pub exq_num: u16,
    pub gapq_num: u16,
}

/// Which DMA channel each queue rides (upstream `struct rtw_rqpn`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rqpn {
    pub dma_map_vo: DmaMapping,
    pub dma_map_vi: DmaMapping,
    pub dma_map_be: DmaMapping,
    pub dma_map_bk: DmaMapping,
    pub dma_map_mg: DmaMapping,
    pub dma_map_hi: DmaMapping,
}

/// Where the reserved and available page counters of one DMA channel live
/// (upstream `struct rtw_prioq_addr`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrioqAddr {
    pub rsvd: u32,
    pub avail: u32,
}

/// `page_table_8822c`; upstream indexes it by bus (0 SDIO, 1 PCIe, 2-4 USB
/// by bulk-out count).
pub const PAGE_TABLE_8822C: [PageTable; 5] = [
    PageTable {
        hq_num: 64,
        nq_num: 64,
        lq_num: 64,
        exq_num: 64,
        gapq_num: 1,
    },
    PageTable {
        hq_num: 64,
        nq_num: 64,
        lq_num: 64,
        exq_num: 64,
        gapq_num: 1,
    },
    PageTable {
        hq_num: 64,
        nq_num: 64,
        lq_num: 0,
        exq_num: 0,
        gapq_num: 1,
    },
    PageTable {
        hq_num: 64,
        nq_num: 64,
        lq_num: 64,
        exq_num: 0,
        gapq_num: 1,
    },
    PageTable {
        hq_num: 64,
        nq_num: 64,
        lq_num: 64,
        exq_num: 64,
        gapq_num: 1,
    },
];

/// The PCIe row of [`PAGE_TABLE_8822C`] (`priority_queue_cfg`, PCIe case).
pub const PAGE_TABLE_PCIE: PageTable = PAGE_TABLE_8822C[1];

use DmaMapping::{Extra, High, Low, Normal};

const fn rqpn(
    vo: DmaMapping,
    vi: DmaMapping,
    be: DmaMapping,
    bk: DmaMapping,
    mg: DmaMapping,
    hi: DmaMapping,
) -> Rqpn {
    Rqpn {
        dma_map_vo: vo,
        dma_map_vi: vi,
        dma_map_be: be,
        dma_map_bk: bk,
        dma_map_mg: mg,
        dma_map_hi: hi,
    }
}

/// `rqpn_table_8822c`, indexed like [`PAGE_TABLE_8822C`].
pub const RQPN_TABLE_8822C: [Rqpn; 5] = [
    rqpn(Normal, Normal, Low, Low, Extra, High),
    rqpn(Normal, Normal, Low, Low, Extra, High),
    rqpn(Normal, Normal, Normal, High, High, High),
    rqpn(Normal, Normal, Low, Low, High, High),
    rqpn(Normal, Normal, Low, Low, Extra, High),
];

/// The PCIe row of [`RQPN_TABLE_8822C`] (`txdma_queue_mapping`, PCIe case).
pub const RQPN_PCIE: Rqpn = RQPN_TABLE_8822C[1];

/// `prioq_addrs_8822c`, indexed by [`DmaMapping`]; the counters are 16 bits
/// wide (`wsize = true`).
pub const PRIOQ_ADDRS_8822C: [PrioqAddr; DMA_MAPPING_MAX] = [
    PrioqAddr {
        rsvd: 0x023C,
        avail: 0x023C + 2,
    }, // Extra: FIFOPAGE_INFO_4
    PrioqAddr {
        rsvd: 0x0234,
        avail: 0x0234 + 2,
    }, // Low: FIFOPAGE_INFO_2
    PrioqAddr {
        rsvd: 0x0238,
        avail: 0x0238 + 2,
    }, // Normal: FIFOPAGE_INFO_3
    PrioqAddr {
        rsvd: 0x0230,
        avail: 0x0230 + 2,
    }, // High: FIFOPAGE_INFO_1
];
pub const PRIOQ_ADDRS_WSIZE: bool = true;

const ALL: u8 = intf::ALL;
const PCI: u8 = intf::PCI;
const USB: u8 = intf::USB;
const SDIO: u8 = intf::SDIO;

const fn w(offset: u16, intf_mask: u8, base: PwrBase, mask: u8, value: u8) -> PwrSeqCmd {
    PwrSeqCmd {
        offset,
        cut_mask: cut::ALL,
        intf_mask,
        base,
        cmd: PwrCmd::Write,
        mask,
        value,
    }
}

const fn p(offset: u16, intf_mask: u8, base: PwrBase, mask: u8, value: u8) -> PwrSeqCmd {
    PwrSeqCmd {
        offset,
        cut_mask: cut::ALL,
        intf_mask,
        base,
        cmd: PwrCmd::Polling,
        mask,
        value,
    }
}

use PwrBase::{Mac, Sdio};

/// `trans_carddis_to_cardemu_8822c`.
pub const TRANS_CARDDIS_TO_CARDEMU_8822C: [PwrSeqCmd; 7] = [
    w(0x0086, SDIO, Sdio, 1 << 0, 0),
    p(0x0086, SDIO, Sdio, 1 << 1, 1 << 1),
    w(0x002E, ALL, Mac, 1 << 2, 1 << 2),
    w(0x002D, ALL, Mac, 1 << 0, 0),
    w(0x007F, ALL, Mac, 1 << 7, 0),
    w(0x004A, USB, Mac, 1 << 0, 0),
    w(0x0005, ALL, Mac, (1 << 3) | (1 << 4) | (1 << 7), 0),
];

/// `trans_cardemu_to_act_8822c`.
pub const TRANS_CARDEMU_TO_ACT_8822C: [PwrSeqCmd; 21] = [
    w(0x0000, USB | SDIO, Mac, 1 << 5, 0),
    w(0x0005, ALL, Mac, (1 << 4) | (1 << 3) | (1 << 2), 0),
    w(0x0075, PCI, Mac, 1 << 0, 1 << 0),
    p(0x0006, ALL, Mac, 1 << 1, 1 << 1),
    w(0x0075, PCI, Mac, 1 << 0, 0),
    w(0xFF1A, USB, Mac, 0xFF, 0),
    w(0x002E, ALL, Mac, 1 << 3, 0),
    w(0x0006, ALL, Mac, 1 << 0, 1 << 0),
    w(0x0005, ALL, Mac, (1 << 4) | (1 << 3), 0),
    w(0x1018, ALL, Mac, 1 << 2, 1 << 2),
    w(0x0005, ALL, Mac, 1 << 0, 1 << 0),
    p(0x0005, ALL, Mac, 1 << 0, 0),
    w(0x0074, PCI, Mac, 1 << 5, 1 << 5),
    w(0x0071, PCI, Mac, 1 << 4, 0),
    w(0x0062, PCI, Mac, 0xE0, 0xE0),
    w(0x0061, PCI, Mac, 0xE0, 0),
    w(0x001F, ALL, Mac, 0xC0, 1 << 7),
    w(0x00EF, ALL, Mac, 0xC0, 1 << 7),
    w(0x1045, ALL, Mac, 1 << 4, 1 << 4),
    w(0x0010, ALL, Mac, 1 << 2, 1 << 2),
    w(0x1064, ALL, Mac, 1 << 1, 1 << 1),
];

/// `trans_act_to_cardemu_8822c`.
pub const TRANS_ACT_TO_CARDEMU_8822C: [PwrSeqCmd; 11] = [
    w(0x0093, ALL, Mac, 1 << 3, 0),
    w(0x001F, ALL, Mac, 0xFF, 0),
    w(0x00EF, ALL, Mac, 0xFF, 0),
    w(0x1045, ALL, Mac, 1 << 4, 0),
    w(0xFF1A, USB, Mac, 0xFF, 0x30),
    w(0x0049, ALL, Mac, 1 << 1, 0),
    w(0x0006, ALL, Mac, 1 << 0, 1 << 0),
    w(0x0002, ALL, Mac, 1 << 1, 0),
    w(0x0005, ALL, Mac, 1 << 1, 1 << 1),
    p(0x0005, ALL, Mac, 1 << 1, 0),
    w(0x0000, USB | SDIO, Mac, 1 << 5, 1 << 5),
];

/// `trans_cardemu_to_carddis_8822c`.
pub const TRANS_CARDEMU_TO_CARDDIS_8822C: [PwrSeqCmd; 11] = [
    w(0x0005, SDIO, Mac, 1 << 7, 1 << 7),
    w(0x0007, USB | SDIO, Mac, 0xFF, 0x00),
    w(0x0067, ALL, Mac, 1 << 5, 0),
    w(0x004A, USB, Mac, 1 << 0, 0),
    w(0x0081, ALL, Mac, (1 << 7) | (1 << 6), 0),
    w(0x0090, ALL, Mac, 1 << 1, 0),
    w(0x0092, PCI, Mac, 0xFF, 0x20),
    w(0x0093, PCI, Mac, 0xFF, 0x04),
    w(0x0005, USB | SDIO, Mac, (1 << 3) | (1 << 4), 1 << 3),
    w(0x0005, PCI, Mac, 1 << 2, 1 << 2),
    w(0x0086, SDIO, Sdio, 1 << 0, 1 << 0),
];

/// `card_enable_flow_8822c` (`chip->pwr_on_seq`).
pub const CARD_ENABLE_FLOW_8822C: [&[PwrSeqCmd]; 2] =
    [&TRANS_CARDDIS_TO_CARDEMU_8822C, &TRANS_CARDEMU_TO_ACT_8822C];

/// `card_disable_flow_8822c` (`chip->pwr_off_seq`).
pub const CARD_DISABLE_FLOW_8822C: [&[PwrSeqCmd]; 2] =
    [&TRANS_ACT_TO_CARDEMU_8822C, &TRANS_CARDEMU_TO_CARDDIS_8822C];
