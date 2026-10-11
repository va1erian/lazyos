//! TX packet descriptors (upstream `tx.c`, `tx.h`): the 48-byte descriptor in
//! front of every packet handed to [`crate::hci::Hci::tx_write`]
//! (`rtw_tx_fill_tx_desc`), its checksum (`fill_txdesc_checksum_common`,
//! `rtw8822c_fill_txdesc_checksum`), and in [`info`] what goes into it for
//! each kind of packet and which queue carries it.
//!
//! The bus never fills a descriptor here (see [`crate::hci`]): the caller
//! builds the packet with [`build_pkt`], or [`info::rsvd_page_pkt`] /
//! [`info::h2c_pkt`], and the bus copies it.

pub mod info;
#[cfg(test)]
mod tests;

use alloc::vec;
use alloc::vec::Vec;

use crate::error::{Error, Result};

pub use info::{
    ac_to_hwq, h2c_pkt, h2c_pkt_info, pci_qsel, pkt_info_update, queue_mapping, rsvd_page_pkt,
    rsvd_page_pkt_info, Ac, RsvdPacket, StaTx, TxParams,
};

/// Bytes of TX packet descriptor on the RTL8822C (`tx_pkt_desc_sz`).
pub const TX_PKT_DESC_SZ: usize = crate::rtw8822c::spec::TX_PKT_DESC_SZ;

/// What to put in a TX descriptor (upstream `struct rtw_tx_pkt_info`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxPktInfo {
    pub tx_pkt_size: u32,
    pub offset: u8,
    pub pkt_offset: u8,
    pub tim_offset: u8,
    pub mac_id: u8,
    pub rate_id: u8,
    pub rate: u8,
    pub qsel: u8,
    pub bw: u8,
    pub sec_type: u8,
    pub sn: u8,
    pub ampdu_en: bool,
    pub ampdu_factor: u8,
    pub ampdu_density: u8,
    pub seq: u16,
    pub stbc: bool,
    pub ldpc: bool,
    pub dis_rate_fallback: bool,
    pub bmc: bool,
    pub use_rate: bool,
    pub ls: bool,
    pub fs: bool,
    pub short_gi: bool,
    pub report: bool,
    pub rts: bool,
    pub dis_qselseq: bool,
    pub en_hwseq: bool,
    pub hw_ssn_sel: u8,
    pub nav_use_hdr: bool,
    pub bt_null: bool,
}

/// Queue selection values (upstream `enum rtw_tx_desc_queue_select`);
/// data frames use their TID, 0-15.
pub const QSEL_BEACON: u8 = 16;
pub const QSEL_HIGH: u8 = 17;
pub const QSEL_MGMT: u8 = 18;
pub const QSEL_H2C: u8 = 19;

/// `sec_type` values (upstream `rtw_tx_pkt_info_update_sec`).
pub const SEC_TYPE_NONE: u8 = 0;
pub const SEC_TYPE_WEP_TKIP: u8 = 0x01;
pub const SEC_TYPE_CCMP: u8 = 0x03;

/// Rates as the descriptors encode them (upstream `enum rtw_trx_desc_rate`,
/// the ones this driver names).
pub const DESC_RATE1M: u8 = 0x00;
pub const DESC_RATE2M: u8 = 0x01;
pub const DESC_RATE5_5M: u8 = 0x02;
pub const DESC_RATE11M: u8 = 0x03;
pub const DESC_RATE6M: u8 = 0x04;
pub const DESC_RATE12M: u8 = 0x06;
pub const DESC_RATE24M: u8 = 0x08;
pub const DESC_RATE54M: u8 = 0x0b;
pub const DESC_RATEMCS0: u8 = 0x0c;
pub const DESC_RATEMCS7: u8 = 0x13;
pub const DESC_RATEMCS15: u8 = 0x1b;
pub const DESC_RATEVHT1SS_MCS0: u8 = 0x2c;
pub const DESC_RATEVHT1SS_MCS7: u8 = 0x33;
pub const DESC_RATEVHT1SS_MCS8: u8 = 0x34;
pub const DESC_RATEVHT1SS_MCS9: u8 = 0x35;
pub const DESC_RATEVHT2SS_MCS7: u8 = 0x3d;
pub const DESC_RATEVHT2SS_MCS8: u8 = 0x3e;
pub const DESC_RATEVHT2SS_MCS9: u8 = 0x3f;
/// One past the last rate (VHT 4SS MCS9 is 0x53).
pub const DESC_RATE_MAX: u8 = 0x54;

/// Rate-adaptation table ids (upstream `enum rtw_rate_id`).
pub const RATEID_BGN_40M_2SS: u8 = 0;
pub const RATEID_BGN_40M_1SS: u8 = 1;
pub const RATEID_BGN_20M_2SS: u8 = 2;
pub const RATEID_BGN_20M_1SS: u8 = 3;
pub const RATEID_GN_N2SS: u8 = 4;
pub const RATEID_GN_N1SS: u8 = 5;
pub const RATEID_BG: u8 = 6;
pub const RATEID_G: u8 = 7;
pub const RATEID_B_20M: u8 = 8;
pub const RATEID_ARFR0_AC_2SS: u8 = 9;
pub const RATEID_ARFR1_AC_1SS: u8 = 10;
pub const RATEID_ARFR2_AC_2G_1SS: u8 = 11;
pub const RATEID_ARFR3_AC_2G_2SS: u8 = 12;

/// Descriptor fields (upstream `RTW_TX_DESC_W*`), as `(word, mask)`.
pub mod field {
    pub const W0_TXPKTSIZE: (usize, u32) = (0, 0xFFFF);
    pub const W0_OFFSET: (usize, u32) = (0, 0xFF << 16);
    pub const W0_BMC: (usize, u32) = (0, 1 << 24);
    pub const W0_LS: (usize, u32) = (0, 1 << 26);
    pub const W0_DISQSELSEQ: (usize, u32) = (0, 1 << 31);
    pub const W1_MACID: (usize, u32) = (1, 0xFF);
    pub const W1_QSEL: (usize, u32) = (1, 0x1F << 8);
    pub const W1_RATE_ID: (usize, u32) = (1, 0x1F << 16);
    pub const W1_SEC_TYPE: (usize, u32) = (1, 0x3 << 22);
    pub const W1_PKT_OFFSET: (usize, u32) = (1, 0x1F << 24);
    pub const W1_MORE_DATA: (usize, u32) = (1, 1 << 29);
    pub const W2_AGG_EN: (usize, u32) = (2, 1 << 12);
    pub const W2_SPE_RPT: (usize, u32) = (2, 1 << 19);
    pub const W2_AMPDU_DEN: (usize, u32) = (2, 0x7 << 20);
    pub const W2_BT_NULL: (usize, u32) = (2, 1 << 23);
    pub const W3_HW_SSN_SEL: (usize, u32) = (3, 0x3 << 6);
    pub const W3_USE_RATE: (usize, u32) = (3, 1 << 8);
    pub const W3_DISDATAFB: (usize, u32) = (3, 1 << 10);
    pub const W3_USE_RTS: (usize, u32) = (3, 1 << 12);
    pub const W3_NAVUSEHDR: (usize, u32) = (3, 1 << 15);
    pub const W3_MAX_AGG_NUM: (usize, u32) = (3, 0x1F << 17);
    pub const W4_DATARATE: (usize, u32) = (4, 0x7F);
    pub const W4_DATARATE_FB_LIMIT: (usize, u32) = (4, 0x1F << 8);
    pub const W4_RTSRATE: (usize, u32) = (4, 0x1F << 24);
    pub const W5_DATA_SHORT: (usize, u32) = (5, 1 << 4);
    pub const W5_DATA_BW: (usize, u32) = (5, 0x3 << 5);
    pub const W5_DATA_LDPC: (usize, u32) = (5, 1 << 7);
    pub const W5_DATA_STBC: (usize, u32) = (5, 0x3 << 8);
    pub const W5_DATA_RTS_SHORT: (usize, u32) = (5, 1 << 12);
    pub const W6_SW_DEFINE: (usize, u32) = (6, 0xFFF);
    pub const W7_TXDESC_CHECKSUM: (usize, u32) = (7, 0xFFFF);
    pub const W7_DMA_TXAGG_NUM: (usize, u32) = (7, 0xFF << 24);
    pub const W8_EN_HWSEQ: (usize, u32) = (8, 1 << 15);
    pub const W9_SW_SEQ: (usize, u32) = (9, 0xFFF << 12);
    pub const W9_TIM_EN: (usize, u32) = (9, 1 << 7);
    pub const W9_TIM_OFFSET: (usize, u32) = (9, 0x7F);
}

/// `le32_encode_bits`: `val` into the field under `mask` (excess bits drop).
fn encode(val: u32, mask: u32) -> u32 {
    (val << mask.trailing_zeros()) & mask
}

/// The descriptor words for `info` (upstream `rtw_tx_fill_tx_desc`; the
/// 8822C has no `old_datarate_fb_limit`).
pub fn tx_desc_words(info: &TxPktInfo) -> [u32; 12] {
    use field::*;
    let mut w = [0u32; 12];
    let mut put = |(i, mask): (usize, u32), val: u32| w[i] |= encode(val, mask);
    let more_data = info.qsel == QSEL_HIGH;
    put(W0_TXPKTSIZE, info.tx_pkt_size);
    put(W0_OFFSET, info.offset.into());
    put(W0_BMC, info.bmc.into());
    put(W0_LS, info.ls.into());
    put(W0_DISQSELSEQ, info.dis_qselseq.into());
    put(W1_MACID, info.mac_id.into());
    put(W1_QSEL, info.qsel.into());
    put(W1_RATE_ID, info.rate_id.into());
    put(W1_SEC_TYPE, info.sec_type.into());
    put(W1_PKT_OFFSET, info.pkt_offset.into());
    put(W1_MORE_DATA, more_data.into());
    put(W2_AGG_EN, info.ampdu_en.into());
    put(W2_SPE_RPT, info.report.into());
    put(W2_AMPDU_DEN, info.ampdu_density.into());
    put(W2_BT_NULL, info.bt_null.into());
    put(W3_HW_SSN_SEL, info.hw_ssn_sel.into());
    put(W3_USE_RATE, info.use_rate.into());
    put(W3_DISDATAFB, info.dis_rate_fallback.into());
    put(W3_USE_RTS, info.rts.into());
    put(W3_NAVUSEHDR, info.nav_use_hdr.into());
    put(W3_MAX_AGG_NUM, info.ampdu_factor.into());
    put(W4_DATARATE, info.rate.into());
    put(W5_DATA_SHORT, info.short_gi.into());
    put(W5_DATA_BW, info.bw.into());
    put(W5_DATA_LDPC, info.ldpc.into());
    put(W5_DATA_STBC, info.stbc.into());
    put(W6_SW_DEFINE, info.sn.into());
    put(W8_EN_HWSEQ, info.en_hwseq.into());
    put(W9_SW_SEQ, info.seq.into());
    if info.rts {
        put(W4_RTSRATE, DESC_RATE24M.into());
        put(W5_DATA_RTS_SHORT, 1);
    }
    if info.tim_offset != 0 {
        put(W9_TIM_EN, 1);
        put(W9_TIM_OFFSET, info.tim_offset.into());
    }
    w
}

/// Upstream `rtw_tx_fill_tx_desc`: write the descriptor for `info` into the
/// first [`TX_PKT_DESC_SZ`] bytes of `desc` (zeroed first, as upstream's
/// `memset`).
pub fn fill_tx_desc(info: &TxPktInfo, desc: &mut [u8]) -> Result<()> {
    let desc = desc
        .get_mut(..TX_PKT_DESC_SZ)
        .ok_or(Error::Invalid("tx: descriptor buffer too short"))?;
    for (chunk, w) in desc
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(tx_desc_words(info))
    {
        chunk.copy_from_slice(&w.to_le_bytes());
    }
    Ok(())
}

/// Upstream `rtw8822c_fill_txdesc_checksum` over
/// `fill_txdesc_checksum_common`: the XOR of the first
/// `(pkt_offset * 8 + 48) / 2` little-endian 16-bit words of `pkt`
/// (descriptor first), into W7's checksum field. Upstream calls it on USB
/// and SDIO only; PCIe leaves the field zero.
pub fn fill_txdesc_checksum(info: &TxPktInfo, pkt: &mut [u8]) -> Result<()> {
    let words = (usize::from(info.pkt_offset) * 8 + TX_PKT_DESC_SZ) / 2;
    if pkt.len() < words * 2 {
        return Err(Error::Invalid("tx: checksum range past the packet"));
    }
    let w7 = 7 * 4;
    pkt[w7] = 0;
    pkt[w7 + 1] = 0;
    let sum = pkt[..words * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .fold(0u16, |acc, c| acc ^ u16::from_le_bytes([c[0], c[1]]));
    pkt[w7..w7 + 2].copy_from_slice(&sum.to_le_bytes());
    Ok(())
}

/// Descriptor and payload as one packet for [`crate::hci::Hci`]: the
/// descriptor from `info` with `tx_pkt_size` set to `payload.len()` (what
/// every upstream caller puts there), then the payload. No checksum: the
/// PCIe path upstream never fills it (see [`fill_txdesc_checksum`]).
pub fn build_pkt(info: &TxPktInfo, payload: &[u8]) -> Vec<u8> {
    let mut info = info.clone();
    info.tx_pkt_size = payload.len() as u32;
    let mut pkt = vec![0u8; TX_PKT_DESC_SZ + payload.len()];
    for (chunk, w) in pkt
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(tx_desc_words(&info))
    {
        chunk.copy_from_slice(&w.to_le_bytes());
    }
    pkt[TX_PKT_DESC_SZ..].copy_from_slice(payload);
    pkt
}
