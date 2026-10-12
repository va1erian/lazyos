//! The 8822C's MAC defaults (upstream `rtw8822c_mac_init` with its
//! `WLAN_*` constants): SIFS/EDCA timings, rate fallback, protocol and
//! aggregation limits, beacon timing, RX filters and the receive
//! configuration, RX power saving and interrupt migration.

use crate::dev::Dev;
use crate::error::Result;
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::util::{clr8, set8};

const WLAN_TXQ_RPT_EN: u8 = 0x1f;
const WLAN_SLOT_TIME: u8 = 0x09;
const WLAN_PIFS_TIME: u8 = 0x1c;
const WLAN_SIFS_CCK_CONT_TX: u32 = 0x0a;
const WLAN_SIFS_OFDM_CONT_TX: u32 = 0x0e;
const WLAN_SIFS_CCK_TRX: u32 = 0x0a;
const WLAN_SIFS_OFDM_TRX: u32 = 0x10;
const WLAN_NAV_MAX: u8 = 0xc8;
const WLAN_RDG_NAV: u32 = 0x05;
const WLAN_TXOP_NAV: u32 = 0x1b;
const WLAN_CCK_RX_TSF: u16 = 0x30;
const WLAN_OFDM_RX_TSF: u16 = 0x30;
const WLAN_TBTT_PROHIBIT: u32 = 0x04;
const WLAN_TBTT_HOLD_TIME: u32 = 0x064;
const WLAN_DRV_EARLY_INT: u8 = 0x04;
const WLAN_BCN_CTRL_CLT0: u8 = 0x10;
const WLAN_BCN_DMA_TIME: u8 = 0x02;
const WLAN_BCN_MAX_ERR: u8 = 0xff;
const WLAN_SIFS_CCK_DUR_TUNE: u16 = 0x0a;
const WLAN_SIFS_OFDM_DUR_TUNE: u16 = 0x10;
const WLAN_SIFS_CCK_CTX: u16 = 0x0a;
const WLAN_SIFS_CCK_IRX: u16 = 0x0a;
const WLAN_SIFS_OFDM_CTX: u16 = 0x0e;
const WLAN_SIFS_OFDM_IRX: u16 = 0x0e;
const WLAN_EIFS_DUR_TUNE: u16 = 0x40;
const WLAN_EDCA_VO_PARAM: u32 = 0x002f_a226;
const WLAN_EDCA_VI_PARAM: u32 = 0x005e_a328;
const WLAN_EDCA_BE_PARAM: u32 = 0x005e_a42b;
const WLAN_EDCA_BK_PARAM: u32 = 0x0000_a44f;
const WLAN_RX_FILTER0: u16 = 0xffff;
const WLAN_RX_FILTER1: u16 = 0xffff;
const WLAN_RX_FILTER2: u16 = 0xffff;
/// Upstream `WLAN_RCR_CFG`.
pub const WLAN_RCR_CFG: u32 = 0xe400_220e;
const WLAN_RXPKT_MAX_SZ_512: u8 = (12288 >> 9) as u8;
const WLAN_AMPDU_MAX_TIME: u8 = 0x70;
const WLAN_RTS_LEN_TH: u32 = 0xff;
const WLAN_RTS_TX_TIME_TH: u32 = 0x08;
const WLAN_MAX_AGG_PKT_LIMIT: u32 = 0x3f;
const WLAN_RTS_MAX_AGG_PKT_LIMIT: u32 = 0x3f;
const WLAN_PRE_TXCNT_TIME_TH: u16 = 0x1e0;
const FAST_EDCA_TH: u8 = 0x06;
const WLAN_BAR_RETRY_LIMIT: u16 = 0x01;
const WLAN_BAR_ACK_TYPE: u8 = 0x05;
const WLAN_RA_TRY_RATE_AGG_LIMIT: u16 = 0x08;
const WLAN_RESP_TXRATE: u8 = 0x84;
const WLAN_ACK_TO: u8 = 0x21;
const WLAN_ACK_TO_CCK: u8 = 0x6a;
const WLAN_MULTI_ADDR: u32 = 0xffff_ffff;
const WLAN_TX_FUNC_CFG1: u8 = 0x30;
const WLAN_TX_FUNC_CFG2: u8 = 0x30;
const WLAN_MAC_OPT_NORM_FUNC1: u8 = 0x98;
const WLAN_MAC_OPT_FUNC2: u32 = 0xb081_0041;
const WLAN_MAC_INT_MIG_CFG: u32 = 0x3333_0000;
const MAC_CLK_SPEED: u8 = 80;

/// Upstream `rtw8822c_mac_init`. The RX filter 1 default is also kept in
/// `hal.rxfltmap1`, as upstream keeps it (the station code edits it).
pub fn mac_init<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let h = &mut dev.hci;
    // TX queue control
    let v = h.read8(REG_FWHW_TXQ_CTRL) | 0x80;
    h.write8(REG_FWHW_TXQ_CTRL, v);
    h.write8(REG_FWHW_TXQ_CTRL + 1, WLAN_TXQ_RPT_EN);
    // SIFS
    h.write16(
        REG_SPEC_SIFS,
        WLAN_SIFS_CCK_DUR_TUNE | (WLAN_SIFS_OFDM_DUR_TUNE << 8),
    );
    h.write32(
        REG_SIFS,
        WLAN_SIFS_CCK_CONT_TX
            | (WLAN_SIFS_OFDM_CONT_TX << 8)
            | (WLAN_SIFS_CCK_TRX << 16)
            | (WLAN_SIFS_OFDM_TRX << 24),
    );
    h.write16(
        REG_RESP_SIFS_CCK,
        WLAN_SIFS_CCK_CTX | (WLAN_SIFS_CCK_IRX << 8),
    );
    h.write16(
        REG_RESP_SIFS_OFDM,
        WLAN_SIFS_OFDM_CTX | (WLAN_SIFS_OFDM_IRX << 8),
    );
    // Rate fallback
    h.write32(REG_DARFRC, 0x0100_0000);
    h.write32(REG_DARFRCH, 0x0807_0504);
    h.write32(REG_RARFRCH, 0x0807_0504);
    h.write32(REG_ARFR0, 0xfe01_f010);
    h.write32(REG_ARFRH0, 0x4000_0000);
    h.write32(REG_ARFR1_V1, 0x003f_f010);
    h.write32(REG_ARFRH1_V1, 0x4000_0000);
    h.write32(REG_ARFR4, 0x0600_f010);
    h.write32(REG_ARFRH4, 0x4000_03e0);
    h.write32(REG_ARFR5, 0x0600_f015);
    h.write32(REG_ARFRH5, 0x0000_00e0);
    // Protocol
    h.write8(REG_AMPDU_MAX_TIME_V1, WLAN_AMPDU_MAX_TIME);
    set8(h, REG_TX_HANG_CTRL, BIT_EN_EOF_V1);
    let pre_txcnt = WLAN_PRE_TXCNT_TIME_TH | BIT_EN_PRECNT as u16;
    h.write8(REG_PRECNT_CTRL, pre_txcnt as u8);
    h.write8(REG_PRECNT_CTRL + 1, (pre_txcnt >> 8) as u8);
    h.write32(
        REG_PROT_MODE_CTRL,
        WLAN_RTS_LEN_TH
            | (WLAN_RTS_TX_TIME_TH << 8)
            | (WLAN_MAX_AGG_PKT_LIMIT << 16)
            | (WLAN_RTS_MAX_AGG_PKT_LIMIT << 24),
    );
    h.write16(
        REG_BAR_MODE_CTRL + 2,
        WLAN_BAR_RETRY_LIMIT | (WLAN_RA_TRY_RATE_AGG_LIMIT << 8),
    );
    h.write8(REG_FAST_EDCA_VOVI_SETTING, FAST_EDCA_TH);
    h.write8(REG_FAST_EDCA_VOVI_SETTING + 2, FAST_EDCA_TH);
    h.write8(REG_FAST_EDCA_BEBK_SETTING, FAST_EDCA_TH);
    h.write8(REG_FAST_EDCA_BEBK_SETTING + 2, FAST_EDCA_TH);
    // Close the BA parser.
    clr8(h, REG_LIFETIME_EN, BIT_BA_PARSER_EN);
    h.clr32(REG_RRSR, BITS_RRSR_RSC);
    // EDCA
    h.write32(REG_EDCA_VO_PARAM, WLAN_EDCA_VO_PARAM);
    h.write32(REG_EDCA_VI_PARAM, WLAN_EDCA_VI_PARAM);
    h.write32(REG_EDCA_BE_PARAM, WLAN_EDCA_BE_PARAM);
    h.write32(REG_EDCA_BK_PARAM, WLAN_EDCA_BK_PARAM);
    h.write8(REG_PIFS, WLAN_PIFS_TIME);
    clr8(h, REG_TX_PTCL_CTRL + 1, BIT_SIFS_BK_EN >> 8);
    set8(
        h,
        REG_RD_CTRL + 1,
        (BIT_DIS_TXOP_CFE | BIT_DIS_LSIG_CFE | BIT_DIS_STBC_CFE) >> 8,
    );
    // MAC clock
    h.clr32(REG_AFE_CTRL1, BIT_MAC_CLK_SEL);
    h.write8(REG_USTIME_TSF, MAC_CLK_SPEED);
    h.write8(REG_USTIME_EDCA, MAC_CLK_SPEED);
    set8(h, REG_MISC_CTRL, BIT_EN_FREE_CNT | BIT_DIS_SECOND_CCA);
    clr8(h, REG_TIMER0_SRC_SEL, BIT_TSFT_SEL_TIMER0);
    h.write16(REG_TXPAUSE, 0x0000);
    h.write8(REG_SLOT, WLAN_SLOT_TIME);
    h.write32(REG_RD_NAV_NXT, WLAN_RDG_NAV | (WLAN_TXOP_NAV << 16));
    h.write16(
        REG_RXTSF_OFFSET_CCK,
        WLAN_CCK_RX_TSF | (WLAN_OFDM_RX_TSF << 8),
    );
    // Beacon: enable the TSF and related functions; send timing.
    set8(h, REG_BCN_CTRL, BIT_EN_BCN_FUNCTION);
    h.write32(
        REG_TBTT_PROHIBIT,
        WLAN_TBTT_PROHIBIT | (WLAN_TBTT_HOLD_TIME << 8),
    );
    h.write8(REG_DRVERLYINT, WLAN_DRV_EARLY_INT);
    h.write8(REG_BCN_CTRL_CLINT0, WLAN_BCN_CTRL_CLT0);
    h.write8(REG_BCNDMATIM, WLAN_BCN_DMA_TIME);
    h.write8(REG_BCN_MAX_ERR, WLAN_BCN_MAX_ERR);
    // WMAC
    h.write32(REG_MAR, WLAN_MULTI_ADDR);
    h.write32(REG_MAR + 4, WLAN_MULTI_ADDR);
    h.write8(REG_BBPSF_CTRL + 2, WLAN_RESP_TXRATE);
    h.write8(REG_ACKTO, WLAN_ACK_TO);
    h.write8(REG_ACKTO_CCK, WLAN_ACK_TO_CCK);
    h.write16(REG_EIFS, WLAN_EIFS_DUR_TUNE);
    h.write8(REG_NAV_CTRL + 2, WLAN_NAV_MAX);
    h.write8(REG_WMAC_TRXPTCL_CTL_H + 2, WLAN_BAR_ACK_TYPE);
    h.write16(REG_RXFLTMAP0, WLAN_RX_FILTER0);
    dev.hal.rxfltmap1 = WLAN_RX_FILTER1;
    h.write16(REG_RXFLTMAP1, WLAN_RX_FILTER1);
    h.write16(REG_RXFLTMAP2, WLAN_RX_FILTER2);
    h.write32(REG_RCR, WLAN_RCR_CFG);
    h.write8(REG_RX_PKT_LIMIT, WLAN_RXPKT_MAX_SZ_512);
    h.write8(REG_TCR + 2, WLAN_TX_FUNC_CFG2);
    h.write8(REG_TCR + 1, WLAN_TX_FUNC_CFG1);
    h.set32(REG_GENERAL_OPTION, BIT_DUMMY_FCS_READY_MASK_EN);
    h.write32(REG_WMAC_OPTION_FUNCTION + 8, WLAN_MAC_OPT_FUNC2);
    h.write8(REG_WMAC_OPTION_FUNCTION_1, WLAN_MAC_OPT_NORM_FUNC1);
    // Low power RX: the RX gated-clock FIFO thresholds (1 for VHT, HT,
    // OFDM, CCK: bits 26/24/22/20 of the dword, seen through its high half).
    let v = (h.read16(REG_RXPSF_CTRL + 2) & 0xf00f) | 0x0550;
    h.write16(REG_RXPSF_CTRL + 2, v);
    // Packet length threshold 1 (bits 15:13), the check enables, OFDM reset.
    let v = (1u16 << 13)
        | (BIT_RXPSF_CTRLEN
            | BIT_RXPSF_VHTCHKEN
            | BIT_RXPSF_HTCHKEN
            | BIT_RXPSF_OFDMCHKEN
            | BIT_RXPSF_CCKCHKEN
            | BIT_RXPSF_OFDMRST) as u16;
    h.write16(REG_RXPSF_CTRL, v);
    h.write32(REG_RXPSF_TYPE_CTRL, 0xffff_ffff);
    // RX ignore configuration: error threshold 7 (bits 2:0).
    let mut v = h.read16(REG_RXPSF_CTRL);
    v &= !((BIT_RXPSF_MHCHKEN | BIT_RXPSF_CCKRST | BIT_RXPSF_CONT_ERRCHKEN) as u16);
    v = (v & !0x7) | 0x7;
    h.write16(REG_RXPSF_CTRL, v);
    set8(h, REG_SND_PTCL_CTRL, BIT_DIS_CHK_VHTSIGB_CRC);
    // Interrupt migration
    h.write32(REG_INT_MIG, WLAN_MAC_INT_MIG_CFG);
    Ok(())
}
