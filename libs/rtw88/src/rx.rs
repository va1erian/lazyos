//! RX descriptors (upstream `rx.c`, `rx.h` and `rtw8822c_query_rx_desc`):
//! the 24-byte RX packet descriptor in front of every received buffer, the
//! driver-info (PHY status) that may follow, and C2H packets that arrive on
//! the same ring.
//!
//! Owner: the core/MAC work package; the PHY status layout itself belongs to
//! the chip ([`crate::rtw8822c`], `query_phy_status`).

use crate::dev::RF_PATH_MAX;

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
