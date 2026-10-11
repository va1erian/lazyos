//! MAC registers and bits (upstream `reg.h`, `mac.h`, `sec.h`), the ones the
//! MAC, firmware, security and station code use. Names are upstream's
//! without the `REG_`/`BIT_` prefix where that reads better in Rust.

pub const SYS_FUNC_EN: u32 = 0x0002;
pub const FEN_CPUEN: u8 = 1 << 2;
pub const FEN_BB_GLB_RST: u8 = 1 << 1;
pub const FEN_BB_RSTB: u8 = 1 << 0;
pub const SYS_PW_CTRL: u32 = 0x0004;
pub const PFM_WOWL: u8 = 1 << 3;
pub const SYS_CLK_CTRL: u32 = 0x0008;
pub const CPU_CLK_EN: u32 = 1 << 14;
pub const RSV_CTRL: u32 = 0x001C;
pub const WLMCU_IOIF: u8 = 1 << 0;
pub const RF_CTRL: u32 = 0x001F;
pub const RF_SDM_RSTB: u8 = 1 << 2;
pub const RF_RSTB: u8 = 1 << 1;
pub const RF_EN: u8 = 1 << 0;
pub const AFE_CTRL1: u32 = 0x0024;
pub const MAC_CLK_SEL: u32 = (1 << 20) | (1 << 21);
pub const SHIFT_MAC_CLK_SEL: u32 = 20;
pub const MAC_CLK_HW_DEF_80M: u32 = 0;
pub const MAC_CLK_SPEED: u8 = 80;
pub const EFUSE_CTRL: u32 = 0x0030;
pub const EF_FLAG: u32 = 1 << 31;
pub const SHIFT_EF_ADDR: u32 = 8;
pub const MASK_EF_ADDR: u32 = 0x3ff;
pub const MASK_EF_DATA: u32 = 0xff;
pub const LDO_EFUSE_CTRL: u32 = 0x0034;
pub const MASK_EFUSE_BANK_SEL: u32 = (1 << 8) | (1 << 9);
pub const GPIO_MUXCFG: u32 = 0x0040;
pub const FSPI_EN: u32 = 1 << 19;
pub const WLRFE_4_5_EN: u32 = 1 << 2;
pub const LED_CFG: u32 = 0x004C;
pub const LNAON_SEL_EN: u32 = 1 << 26;
pub const PAPE_SEL_EN: u32 = 1 << 25;
pub const PAD_CTRL1: u32 = 0x0064;
pub const PAPE_WLBT_SEL: u32 = 1 << 29;
pub const LNAON_WLBT_SEL: u32 = 1 << 28;
pub const HCI_OPT_CTRL: u32 = 0x0074;
pub const USB_SUS_DIS: u32 = 1 << 8;
pub const MCUFW_CTRL: u32 = 0x0080;
pub const BOOT_FSPI_EN: u32 = 1 << 20;
pub const FW_INIT_RDY: u32 = 1 << 15;
pub const FW_DW_RDY: u32 = 1 << 14;
pub const CPU_CLK_SEL: u32 = (1 << 12) | (1 << 13);
pub const RPWM_TOGGLE: u8 = 1 << 7;
pub const DMEM_CHKSUM_OK: u32 = 1 << 6;
pub const DMEM_DW_OK: u32 = 1 << 5;
pub const IMEM_CHKSUM_OK: u32 = 1 << 4;
pub const IMEM_DW_OK: u32 = 1 << 3;
pub const MCUFWDL_EN: u32 = 1 << 0;
pub const CHECK_SUM_OK: u32 = IMEM_CHKSUM_OK | DMEM_CHKSUM_OK;
pub const FW_READY: u32 = FW_INIT_RDY | FW_DW_RDY | IMEM_DW_OK | DMEM_DW_OK | CHECK_SUM_OK;
pub const FW_READY_MASK: u32 = 0xffff & !CPU_CLK_SEL;
pub const MCU_TST_CFG: u32 = 0x84;
pub const VAL_FW_TRIGGER: u8 = 0x1;
pub const WLRF1: u32 = 0x00EC;
pub const WLRF1_BBRF_EN: u32 = (1 << 24) | (1 << 25) | (1 << 26);
pub const SYS_CFG1: u32 = 0x00F0;
pub const RTL_ID: u32 = 1 << 23;
pub const RF_TYPE_ID: u32 = 1 << 27;
pub const SHIFT_CHIP_VER: u32 = 12;
pub const MASK_CHIP_VER: u32 = 0xf;
pub const CR: u32 = 0x0100;
pub const MAC_SEC_EN: u16 = 1 << 9;
pub const ENSWBCN: u16 = 1 << 8;
pub const HCI_TXDMA_EN: u8 = 1 << 0;
pub const HCI_RXDMA_EN: u8 = 1 << 1;
pub const TXDMA_EN: u8 = 1 << 2;
pub const RXDMA_EN: u8 = 1 << 3;
pub const PROTOCOL_EN: u8 = 1 << 4;
pub const SCHEDULE_EN: u8 = 1 << 5;
pub const MACTXEN: u8 = 1 << 6;
pub const MACRXEN: u8 = 1 << 7;
pub const MAC_TRX_ENABLE: u8 = HCI_TXDMA_EN
    | HCI_RXDMA_EN
    | TXDMA_EN
    | RXDMA_EN
    | PROTOCOL_EN
    | SCHEDULE_EN
    | MACTXEN
    | MACRXEN;
pub const TXDMA_PQ_MAP: u32 = 0x010C;
pub const TRXFF_BNDY: u32 = 0x0114;
pub const RXFF_BNDY: u32 = 0x011C;
pub const C2HEVT: u32 = 0x01A0;
pub const HMETFR: u32 = 0x01CC;
pub const HMEBOX: [u32; 4] = [0x01D0, 0x01D4, 0x01D8, 0x01DC];
pub const HMEBOX_EX: [u32; 4] = [0x01F0, 0x01F4, 0x01F8, 0x01FC];
pub const FIFOPAGE_CTRL_2: u32 = 0x0204;
pub const BCN_VALID_V1: u16 = 1 << 15;
pub const MASK_BCN_HEAD_1_V1: u16 = 0xfff;
pub const AUTO_LLT_V1: u32 = 0x0208;
pub const AUTO_INIT_LLT_V1: u8 = 1 << 0;
pub const TXDMA_OFFSET_CHK: u32 = 0x020C;
pub const TXDMA_STATUS: u32 = 0x0210;
pub const PAGE_OVF: u32 = 1 << 2;
pub const RQPN_CTRL_2: u32 = 0x022C;
pub const LD_RQPN: u32 = 1 << 31;
pub const FIFOPAGE_INFO: [u32; 5] = [0x0230, 0x0234, 0x0238, 0x023C, 0x0240];
pub const H2C_HEAD: u32 = 0x0244;
pub const H2C_TAIL: u32 = 0x0248;
pub const H2C_READ_ADDR: u32 = 0x024C;
pub const H2C_INFO: u32 = 0x0254;
pub const FWHW_TXQ_CTRL: u32 = 0x0420;
pub const EN_BCNQ_DL: u32 = 1 << 22;
pub const EN_WR_FREE_TAIL: u32 = 1 << 20;
pub const BCNQ_BDNY_V1: u32 = 0x0424;
pub const BCNQ1_BDNY_V1: u32 = 0x0456;
pub const CCK_CHECK: u32 = 0x0454;
pub const CHECK_CCK_EN: u8 = 1 << 7;
pub const DATA_SC: u32 = 0x0483;
pub const BCN_CTRL: u32 = 0x0550;
pub const DIS_TSF_UDT: u8 = 1 << 4;
pub const EN_BCN_FUNCTION: u8 = 1 << 3;
pub const USTIME_TSF: u32 = 0x055C;
pub const DTIM_COUNTER_ROOT: u32 = 0x05A8;
pub const TCR: u32 = 0x0604;
pub const TCR_UPDATE_TIMIE: u32 = 1 << 5;
pub const RCR: u32 = 0x0608;
pub const RX_DRVINFO_SZ: u32 = 0x060F;
pub const USTIME_EDCA: u32 = 0x0638;
pub const WMAC_TRXPTCL_CTL: u32 = 0x0668;
pub const RFMOD: u32 = (1 << 7) | (1 << 8);
pub const RFMOD_80M: u32 = 1 << 8;
pub const RFMOD_40M: u32 = 1 << 7;
pub const RXFLTMAP0: u32 = 0x06A0;
pub const RXFLTMAP1: u32 = 0x06A2;
pub const RXFLTMAP2: u32 = 0x06A4;
pub const WMAC_OPTION_FUNCTION: u32 = 0x07D0;
pub const CPU_DMEM_CON: u32 = 0x1080;
pub const WL_PLATFORM_RST: u32 = 1 << 16;
pub const DDMA_EN: u32 = 1 << 8;
pub const H2C_PKT_READADDR: u32 = 0x10D0;
pub const H2C_PKT_WRITEADDR: u32 = 0x10D4;
pub const FW_DBG7: u32 = 0x10FC;
pub const FW_KEY_MASK: u32 = 0xffff_ff00;
pub const ILLEGAL_KEY_GROUP: u32 = 0xFAAA_AA00;
pub const CR_EXT: u32 = 0x1100;
pub const DDMA_CH0SA: u32 = 0x1200;
pub const DDMA_CH0DA: u32 = 0x1204;
pub const DDMA_CH0CTRL: u32 = 0x1208;
pub const DDMACH0_OWN: u32 = 1 << 31;
pub const DDMACH0_CHKSUM_EN: u32 = 1 << 29;
pub const DDMACH0_CHKSUM_STS: u32 = 1 << 27;
pub const DDMACH0_DDMA_MODE: u32 = 1 << 26;
pub const DDMACH0_RESET_CHKSUM_STS: u32 = 1 << 25;
pub const DDMACH0_CHKSUM_CONT: u32 = 1 << 24;
pub const MASK_DDMACH0_DLEN: u32 = 0x3ffff;
pub const H2CQ_CSR: u32 = 0x1330;
pub const H2CQ_FULL: u32 = 1 << 31;
pub const LTECOEX_READY: u32 = 1 << 29;

// TXDMA_PQ_MAP fields (`BIT_TXDMA_*_MAP`).
pub const fn txdma_voq_map(x: u16) -> u16 {
    (x & 0x3) << 4
}
pub const fn txdma_viq_map(x: u16) -> u16 {
    (x & 0x3) << 6
}
pub const fn txdma_beq_map(x: u16) -> u16 {
    (x & 0x3) << 8
}
pub const fn txdma_bkq_map(x: u16) -> u16 {
    (x & 0x3) << 10
}
pub const fn txdma_mgq_map(x: u16) -> u16 {
    (x & 0x3) << 12
}
pub const fn txdma_hiq_map(x: u16) -> u16 {
    (x & 0x3) << 14
}

// RCR bits (`REG_RCR`).
pub const RCR_APP_FCS: u32 = 1 << 31;
pub const RCR_APP_MIC: u32 = 1 << 30;
pub const RCR_APP_ICV: u32 = 1 << 29;
pub const RCR_APP_PHYSTS: u32 = 1 << 28;
pub const RCR_VHT_DACK: u32 = 1 << 26;
pub const RCR_PKTCTL_DLEN: u32 = 1 << 20;
pub const RCR_HTC_LOC_CTRL: u32 = 1 << 14;
pub const RCR_ACRC32: u32 = 1 << 8;
pub const RCR_CBSSID_BCN: u32 = 1 << 7;
pub const RCR_CBSSID_DATA: u32 = 1 << 6;
pub const RCR_AB: u32 = 1 << 3;
pub const RCR_AM: u32 = 1 << 2;
pub const RCR_APM: u32 = 1 << 1;
pub const RCR_AAP: u32 = 1 << 0;

// mac.h
pub const C2H_PKT_BUF: u32 = 256;
pub const PHY_STATUS_SIZE: u8 = 4;
pub const OCPBASE_RXBUF_FW_88XX: u32 = 0x1868_0000;
pub const OCPBASE_TXBUF_88XX: u32 = 0x1878_0000;
pub const OCPBASE_DMEM_88XX: u32 = 0x0020_0000;
pub const RSVD_PG_H2C_EXTRAINFO_NUM: u32 = 24;
pub const RSVD_PG_H2C_STATICINFO_NUM: u32 = 8;
pub const RSVD_PG_H2CQ_NUM: u32 = 8;
pub const RSVD_PG_CPU_INSTRUCTION_NUM: u32 = 0;
pub const RSVD_PG_FW_TXBUF_NUM: u32 = 4;
/// Upstream `RTW_PWR_POLLING_CNT`: polls of 50 us in a power sequence.
pub const PWR_POLLING_CNT: u32 = 20_000;
