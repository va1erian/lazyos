//! RX descriptors (upstream `rx.c`, `rx.h`: `rtw_rx_query_rx_desc`, which
//! the RTL8822C uses as its `query_rx_desc`): the 24-byte RX packet
//! descriptor in front of every received buffer, the driver info (PHY
//! status) that may follow, and C2H packets that arrive on the same ring.
//!
//! The PHY status layout itself belongs to the chip
//! (`crate::rtw8822c::query_phy_status`).
//!
//! Everything here is read from a buffer the device wrote, so every field
//! is checked before it is used: a corrupt or hostile buffer yields
//! [`Error::Invalid`], never a panic or a slice outside the input.

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
#[cfg(test)]
mod tests;

use crate::dev::RF_PATH_MAX;
use crate::error::{Error, Result};
use crate::rtw8822c::spec::RX_PKT_DESC_SZ;
use crate::rtw8822c::PhyStatusCtx;
use crate::tx::DESC_RATE_MAX;

/// What a received packet's descriptor and PHY status say (upstream
/// `struct rtw_rx_pkt_stat`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RxPktStat {
    pub phy_status: bool,
    pub icv_err: bool,
    pub crc_err: bool,
    pub decrypted: bool,
    pub is_c2h: bool,
    pub channel_invalid: bool,

    pub signal_power: i32,
    pub pkt_len: u16,
    pub bw: u8,
    pub drv_info_sz: u8,
    pub shift: u8,
    pub rate: u8,
    pub mac_id: u8,
    pub cam_id: u8,
    pub ppdu_cnt: u8,
    pub tsf_low: u32,
    pub rx_power: [i8; RF_PATH_MAX],
    pub rssi: u8,
    pub rxsc: u8,
    pub rx_snr: [i8; RF_PATH_MAX],
    pub rx_evm: [u8; RF_PATH_MAX],
    pub cfo_tail: [i8; RF_PATH_MAX],
    /// Frequency in MHz of the channel it arrived on.
    pub freq: u16,
    pub band: u8,
}

/// Encryption types in the descriptor (upstream `enum rtw_rx_desc_enc`).
pub const RX_DESC_ENC_NONE: u32 = 0;
pub const RX_DESC_ENC_WEP40: u32 = 1;
pub const RX_DESC_ENC_TKIP_WO_MIC: u32 = 2;
pub const RX_DESC_ENC_TKIP_MIC: u32 = 3;
pub const RX_DESC_ENC_AES: u32 = 4;
pub const RX_DESC_ENC_WEP104: u32 = 5;

/// Descriptor fields (upstream `RTW_RX_DESC_*`).
pub const W0_PKT_LEN: u32 = 0x3FFF;
pub const W0_CRC32: u32 = 1 << 14;
pub const W0_ICV_ERR: u32 = 1 << 15;
pub const W0_DRV_INFO_SIZE: u32 = 0xF << 16;
pub const W0_ENC_TYPE: u32 = 0x7 << 20;
pub const W0_SHIFT: u32 = 0x3 << 24;
pub const W0_PHYST: u32 = 1 << 26;
pub const W0_SWDEC: u32 = 1 << 27;
pub const W1_MACID: u32 = 0x7F;
pub const W2_C2H: u32 = 1 << 28;
pub const W2_PPDU_CNT: u32 = 0x3 << 29;
pub const W3_RX_RATE: u32 = 0x7F;
pub const W4_BW: u32 = 0x3 << 4;

/// The driver-info size the MAC is programmed with, in 8-byte units
/// (upstream `PHY_STATUS_SIZE`, `mac.c` writes it to `REG_RX_DRVINFO_SZ`).
pub const PHY_STATUS_SIZE: u32 = 4;
/// The longest MPDU accepted (`IEEE80211_MAX_MPDU_LEN_VHT_11454`).
pub const MAX_MPDU_LEN: u32 = 11454;
/// The frame check sequence the chip leaves on every frame.
pub const FCS_LEN: u32 = 4;

/// Where the payload sits in a received buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxLayout {
    /// Bytes before the payload: descriptor, driver info and shift
    /// (upstream `pkt_offset`).
    pub offset: usize,
    /// Bytes of payload (`pkt_len`): the 802.11 frame with its FCS, or the
    /// C2H packet.
    pub len: usize,
    pub is_c2h: bool,
}

impl RxLayout {
    /// One past the last byte the buffer must hold.
    pub fn end(&self) -> usize {
        self.offset + self.len
    }

    /// The payload in `buf` (the buffer [`query_rx_desc`] accepted).
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        buf.get(self.offset..self.end()).unwrap_or(&[])
    }
}

fn field(word: u32, mask: u32) -> u32 {
    (word & mask) >> mask.trailing_zeros()
}

fn word(buf: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([buf[4 * i], buf[4 * i + 1], buf[4 * i + 2], buf[4 * i + 3]])
}

/// Bytes the buffer described by the 24-byte descriptor at the start of
/// `desc` spans: descriptor, driver info, shift and payload. The bus uses
/// it to know how much of a slot to copy; it checks nothing else.
pub fn rx_desc_extent(desc: &[u8]) -> Result<usize> {
    if desc.len() < RX_PKT_DESC_SZ {
        return Err(Error::Invalid("rx: short descriptor"));
    }
    let w0 = word(desc, 0);
    let drv_info = field(w0, W0_DRV_INFO_SIZE) as usize * 8;
    let shift = field(w0, W0_SHIFT) as usize;
    let pkt_len = field(w0, W0_PKT_LEN) as usize;
    Ok(RX_PKT_DESC_SZ + drv_info + shift + pkt_len)
}

/// Upstream `rtw_rx_query_rx_desc`: fill `stat` from the buffer `buf`
/// (descriptor first) and say where its payload is. The PHY status, when
/// present, goes to the chip's `query_phy_status` with `ctx` (a PHY
/// status the chip does not recognise leaves the PHY fields at their
/// defaults; upstream warns and keeps the frame too).
pub fn query_rx_desc(ctx: &PhyStatusCtx, buf: &[u8], stat: &mut RxPktStat) -> Result<RxLayout> {
    query_rx_desc_with(buf, stat, |ps, st| {
        let _ = crate::rtw8822c::query_phy_status(ctx, ps, st);
    })
}

/// [`query_rx_desc`] with the PHY status parser given (tests, fuzzing).
pub fn query_rx_desc_with(
    buf: &[u8],
    stat: &mut RxPktStat,
    phy: impl FnOnce(&[u8], &mut RxPktStat),
) -> Result<RxLayout> {
    *stat = RxPktStat::default();
    if buf.len() < RX_PKT_DESC_SZ {
        return Err(Error::Invalid("rx: short descriptor"));
    }
    let (w0, w1, w2, w3, w4, w5) = (
        word(buf, 0),
        word(buf, 1),
        word(buf, 2),
        word(buf, 3),
        word(buf, 4),
        word(buf, 5),
    );
    stat.pkt_len = field(w0, W0_PKT_LEN) as u16;
    stat.crc_err = w0 & W0_CRC32 != 0;
    stat.icv_err = w0 & W0_ICV_ERR != 0;
    let drv_info_units = field(w0, W0_DRV_INFO_SIZE);
    let enc_type = field(w0, W0_ENC_TYPE);
    stat.shift = field(w0, W0_SHIFT) as u8;
    stat.phy_status = w0 & W0_PHYST != 0;
    let swdec = w0 & W0_SWDEC != 0;
    stat.decrypted = !swdec && enc_type != RX_DESC_ENC_NONE;
    stat.cam_id = field(w1, W1_MACID) as u8;
    stat.is_c2h = w2 & W2_C2H != 0;
    stat.ppdu_cnt = field(w2, W2_PPDU_CNT) as u8;
    stat.rate = field(w3, W3_RX_RATE) as u8;
    stat.bw = field(w4, W4_BW) as u8;
    stat.tsf_low = w5;

    if stat.rate >= DESC_RATE_MAX {
        return Err(Error::Invalid("rx: rate"));
    }
    if drv_info_units != 0 && drv_info_units != PHY_STATUS_SIZE {
        return Err(Error::Invalid("rx: driver info size"));
    }
    if stat.phy_status && drv_info_units == 0 {
        return Err(Error::Invalid("rx: phy status without driver info"));
    }
    if u32::from(stat.pkt_len) > MAX_MPDU_LEN {
        return Err(Error::Invalid("rx: packet length"));
    }
    // drv_info_sz is in units of 8 bytes.
    stat.drv_info_sz = (drv_info_units * 8) as u8;

    let phy_at = RX_PKT_DESC_SZ + usize::from(stat.shift);
    let layout = RxLayout {
        offset: phy_at + usize::from(stat.drv_info_sz),
        len: usize::from(stat.pkt_len),
        is_c2h: stat.is_c2h,
    };
    // Upstream trusts the buffer to be as long as the descriptor says; here
    // that is the first thing checked about the payload.
    if layout.end() > buf.len() {
        return Err(Error::Invalid(
            "rx: buffer shorter than the descriptor says",
        ));
    }
    // A C2H packet's RX and PHY status are of no interest.
    if stat.is_c2h {
        return Ok(layout);
    }
    if u32::from(stat.pkt_len) <= FCS_LEN {
        return Err(Error::Invalid("rx: packet shorter than its FCS"));
    }
    if stat.phy_status {
        phy(&buf[phy_at..layout.offset], stat);
    }
    Ok(layout)
}
