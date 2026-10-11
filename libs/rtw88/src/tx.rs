//! TX packet descriptors (upstream `tx.c`, `tx.h`): the 48-byte descriptor in
//! front of every packet handed to [`crate::hci::Hci::tx_write`], and the
//! queue selection (`rtw_tx_ac_to_hwq`, `rtw_tx_queue_mapping`).
//!
//! Owner: the core/MAC work package.

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
