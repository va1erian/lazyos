//! What goes into a TX descriptor for each kind of packet, and which queue
//! carries it (upstream `tx.c`: `rtw_tx_pkt_info_update` and its `_mgmt_`,
//! `_data_`, `_rate`, `_sec` helpers, `rtw_tx_rsvd_page_pkt_info_update`,
//! `rtw_tx_write_data_rsvd_page_get`, `rtw_tx_write_data_h2c_get`,
//! `rtw_tx_ac_to_hwq`, `rtw_tx_queue_mapping`; `pci.c`:
//! `rtw_pci_get_tx_qsel`).
//!
//! Upstream reads the frame's `ieee80211_tx_info` and station; here the
//! soft-MAC passes what those would say in [`TxParams`]. The frame itself
//! is read only for its type, `addr1` and sequence number.

use alloc::vec::Vec;

use crate::dev::Band;
use crate::error::{Error, Result};
use crate::hci::TxQueue;

use super::*;

/// Access categories in mac80211's numbering (`IEEE80211_AC_*`), which is
/// the skb queue mapping upstream's tables are indexed by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Ac {
    Vo = 0,
    Vi = 1,
    #[default]
    Be = 2,
    Bk = 3,
}

impl Ac {
    /// The access category of 802.1D user priority `tid` (IEEE 802.11
    /// table 10-1; TIDs 8-15 are not user priorities and go best effort).
    pub fn from_tid(tid: u8) -> Ac {
        match tid {
            1 | 2 => Ac::Bk,
            4 | 5 => Ac::Vi,
            6 | 7 => Ac::Vo,
            _ => Ac::Be,
        }
    }
}

/// Upstream `rtw_tx_ac_to_hwq`.
pub fn ac_to_hwq(ac: Ac) -> TxQueue {
    match ac {
        Ac::Vo => TxQueue::Vo,
        Ac::Vi => TxQueue::Vi,
        Ac::Be => TxQueue::Be,
        Ac::Bk => TxQueue::Bk,
    }
}

/// The 802.11 header facts upstream's TX path looks at.
struct Hdr {
    ftype: u8,
    subtype: u8,
    bmc: bool,
}

impl Hdr {
    /// Frame control and `addr1`, which every frame has (an ACK is 10 bytes).
    fn parse(frame: &[u8]) -> Result<Hdr> {
        if frame.len() < 10 {
            return Err(Error::Invalid("tx: frame shorter than its header"));
        }
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        Ok(Hdr {
            ftype: ((fc >> 2) & 0x3) as u8,
            subtype: ((fc >> 4) & 0xF) as u8,
            // A broadcast address is a multicast one: the group bit.
            bmc: frame[4] & 1 != 0,
        })
    }
    fn is_mgmt(&self) -> bool {
        self.ftype == 0
    }
    fn is_ctl(&self) -> bool {
        self.ftype == 1
    }
    fn is_data(&self) -> bool {
        self.ftype == 2
    }
    fn is_beacon(&self) -> bool {
        self.is_mgmt() && self.subtype == 8
    }
    /// Null or QoS Null data.
    fn is_any_nullfunc(&self) -> bool {
        self.is_data() && (self.subtype == 4 || self.subtype == 12)
    }
}

/// Upstream `rtw_tx_queue_mapping`: the hardware queue for `frame`, whose
/// skb queue mapping is `ac`.
pub fn queue_mapping(frame: &[u8], ac: Ac) -> Result<TxQueue> {
    let hdr = Hdr::parse(frame)?;
    Ok(if hdr.is_beacon() {
        TxQueue::Bcn
    } else if hdr.is_mgmt() || hdr.is_ctl() {
        TxQueue::Mgmt
    } else if hdr.bmc {
        TxQueue::Hi0
    } else {
        ac_to_hwq(ac)
    })
}

/// Upstream `rtw_pci_get_tx_qsel`: the descriptor's queue select for a
/// packet on `queue`, `priority` being the skb priority (the TID).
pub fn pci_qsel(queue: TxQueue, priority: u8) -> u8 {
    match queue {
        TxQueue::Bcn => QSEL_BEACON,
        TxQueue::H2c => QSEL_H2C,
        TxQueue::Mgmt => QSEL_MGMT,
        TxQueue::Hi0 => QSEL_HIGH,
        _ => priority,
    }
}

/// The rate a station sends data at, decided by the soft-MAC from the
/// peer's capabilities (upstream reads `struct rtw_sta_info`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StaTx {
    pub rate: u8,
    pub rate_id: u8,
    pub bw: u8,
    pub stbc: bool,
    pub ldpc: bool,
    /// RTS/CTS first (`control.use_rts`, or longer than the RTS threshold).
    pub rts: bool,
    /// `(ampdu_factor, ampdu_density)` when the frame is part of an A-MPDU.
    pub ampdu: Option<(u8, u8)>,
}

/// Everything about a frame upstream takes from mac80211.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxParams {
    /// The station's or the interface's MAC id.
    pub mac_id: u8,
    pub band: Band,
    /// The BSS basic-rate bitmap (bit 0 the lowest rate of the band); 0
    /// sends management frames at the band's lowest rate.
    pub basic_rates: u32,
    /// The skb queue mapping.
    pub ac: Ac,
    /// The skb priority (TID).
    pub tid: u8,
    /// [`SEC_TYPE_CCMP`] when the hardware encrypts with a CAM key.
    pub sec_type: u8,
    /// The peer, for unicast data; `None` for group frames.
    pub sta: Option<StaTx>,
    /// Ask the firmware for a TX report with this sequence number
    /// ([`report_sn`]).
    pub report_sn: Option<u8>,
    /// A fixed data rate (upstream `dm_info.fix_rate`, debugfs).
    pub fix_rate: Option<u8>,
}

/// The `sn` of the `n`th TX report (upstream `rtw_tx_report_enable`: bits
/// 7:2, the rest the firmware's).
pub fn report_sn(n: u8) -> u8 {
    (n << 2) & 0xfc
}

/// Upstream `get_tx_ampdu_factor`, from the peer's HT A-MPDU length exponent.
pub fn ampdu_factor(exp: u8) -> u8 {
    ((4u32 << exp.min(3)) - 1) as u8
}

/// Upstream `get_highest_ht_tx_rate`.
pub fn highest_ht_tx_rate(rf_2t2r: bool, peer_mcs_mask1: u8) -> u8 {
    if rf_2t2r && peer_mcs_mask1 != 0 {
        DESC_RATEMCS15
    } else {
        DESC_RATEMCS7
    }
}

/// Upstream `get_highest_vht_tx_rate`, from our spatial streams and the
/// peer's VHT TX MCS map.
pub fn highest_vht_tx_rate(nss: u8, tx_mcs_map: u16) -> u8 {
    let pick = |field: u16, mcs7, mcs8, mcs9| match field {
        0 => mcs7,
        1 => mcs8,
        _ => mcs9,
    };
    match nss {
        1 => pick(
            tx_mcs_map & 0x3,
            DESC_RATEVHT1SS_MCS7,
            DESC_RATEVHT1SS_MCS8,
            DESC_RATEVHT1SS_MCS9,
        ),
        n if n >= 2 => pick(
            (tx_mcs_map & 0xc) >> 2,
            DESC_RATEVHT2SS_MCS7,
            DESC_RATEVHT2SS_MCS8,
            DESC_RATEVHT2SS_MCS9,
        ),
        _ => DESC_RATEVHT1SS_MCS9,
    }
}

/// Upstream's legacy choice: 11 Mb/s for a CCK-only peer (its band-0
/// supported-rate bitmap fits in four bits), else 54 Mb/s.
pub fn legacy_tx_rate(supp_rates0: u32) -> u8 {
    if supp_rates0 <= 0xf {
        DESC_RATE11M
    } else {
        DESC_RATE54M
    }
}

/// Upstream `rtw_tx_pkt_info_update_rate` with `rtw_get_mgmt_rate`.
fn update_rate(info: &mut TxPktInfo, band: Band, basic_rates: u32, ignore_rate: bool) {
    let (rate_id, lowest) = match band {
        Band::Ghz2 => (RATEID_B_20M, DESC_RATE1M),
        Band::Ghz5 => (RATEID_G, DESC_RATE6M),
    };
    info.rate_id = rate_id;
    info.rate = if basic_rates == 0 || ignore_rate {
        lowest
    } else {
        basic_rates.trailing_zeros() as u8 + lowest
    };
    info.use_rate = true;
    info.dis_rate_fallback = true;
}

/// Upstream `rtw_tx_mgmt_pkt_info_update`: basic rate, no fallback, the
/// hardware's sequence numbers.
pub fn mgmt_pkt_info(info: &mut TxPktInfo, band: Band, basic_rates: u32) {
    update_rate(info, band, basic_rates, false);
    info.dis_qselseq = true;
    info.en_hwseq = true;
    info.hw_ssn_sel = 0;
}

/// Upstream `rtw_tx_data_pkt_info_update`. `frame` must hold a full data
/// header (24 bytes) for its sequence number.
pub fn data_pkt_info(
    info: &mut TxPktInfo,
    frame: &[u8],
    sta: Option<&StaTx>,
    fix_rate: Option<u8>,
) -> Result<()> {
    let seq_ctrl = frame
        .get(22..24)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(Error::Invalid("tx: data frame shorter than its header"))?;
    info.seq = (seq_ctrl & 0xFFF0) >> 4;
    // Group frames: the defaults.
    let (mut rate, mut rate_id, mut bw, mut stbc, mut ldpc) =
        (DESC_RATE6M, RATEID_BG, 0, false, false);
    let (mut ampdu_en, mut factor, mut density) = (false, 0, 0);
    if let Some(sta) = sta {
        if let Some((f, d)) = sta.ampdu {
            (ampdu_en, factor, density) = (true, f, d);
        }
        if sta.rts {
            info.rts = true;
        }
        (rate, rate_id, bw, stbc, ldpc) = (sta.rate, sta.rate_id, sta.bw, sta.stbc, sta.ldpc);
    }
    info.ampdu_factor = factor;
    info.ampdu_density = density;
    info.ampdu_en = ampdu_en;
    info.rate = rate;
    info.rate_id = rate_id;
    info.bw = bw;
    info.stbc = stbc;
    info.ldpc = ldpc;
    if let Some(fix) = fix_rate.filter(|r| *r < DESC_RATE_MAX) {
        info.rate = fix;
        info.dis_rate_fallback = true;
        info.use_rate = true;
    }
    Ok(())
}

/// Upstream `rtw_tx_pkt_info_update`, plus the queue (`rtw_tx_queue_mapping`)
/// and the queue select the PCIe bus would put in (`rtw_pci_get_tx_qsel`):
/// everything [`super::build_pkt`] needs for a frame from the soft-MAC.
pub fn pkt_info_update(frame: &[u8], p: &TxParams) -> Result<(TxQueue, TxPktInfo)> {
    let hdr = Hdr::parse(frame)?;
    let mut info = TxPktInfo {
        mac_id: p.mac_id,
        ..TxPktInfo::default()
    };
    if hdr.is_mgmt() || hdr.is_any_nullfunc() {
        mgmt_pkt_info(&mut info, p.band, p.basic_rates);
    } else if hdr.is_data() {
        data_pkt_info(&mut info, frame, p.sta.as_ref(), p.fix_rate)?;
    }
    if let Some(sn) = p.report_sn {
        info.sn = sn;
        info.report = true;
    }
    info.bmc = hdr.bmc;
    info.sec_type = p.sec_type;
    info.tx_pkt_size = frame.len() as u32;
    info.offset = TX_PKT_DESC_SZ as u8;
    info.ls = true;
    let queue = queue_mapping(frame, p.ac)?;
    info.qsel = pci_qsel(queue, p.tid);
    Ok((queue, info))
}

/// Reserved-page packet types (upstream `enum rtw_rsvd_packet_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RsvdPacket {
    Beacon,
    Dummy,
    PsPoll,
    ProbeResp,
    Null,
    QosNull,
    LpsPgDpk,
    LpsPgInfo,
    ProbeReq,
    NloInfo,
    ChInfo,
}

/// Upstream `rtw_tx_rsvd_page_pkt_info_update` for `frame` of type `kind`.
/// A beacon or dummy packet is the first reserved page and goes through the
/// beacon queue, so it gets [`QSEL_BEACON`] here (upstream leaves it to the
/// bus, which sets exactly that); the others get [`QSEL_MGMT`]. No TIM
/// offset: this driver is a station.
pub fn rsvd_page_pkt_info(frame: &[u8], kind: RsvdPacket, band: Band) -> TxPktInfo {
    let mut info = TxPktInfo {
        qsel: match kind {
            RsvdPacket::Beacon | RsvdPacket::Dummy => QSEL_BEACON,
            _ => QSEL_MGMT,
        },
        ..TxPktInfo::default()
    };
    update_rate(&mut info, band, 0, true);
    // Upstream reads addr1 even when the "frame" is a firmware chunk.
    info.bmc = frame.get(4).is_some_and(|b| b & 1 != 0);
    info.tx_pkt_size = frame.len() as u32;
    info.offset = TX_PKT_DESC_SZ as u8;
    info.ls = true;
    if kind == RsvdPacket::PsPoll {
        info.nav_use_hdr = true;
    } else {
        info.dis_qselseq = true;
        info.en_hwseq = true;
        info.hw_ssn_sel = 0;
    }
    if kind == RsvdPacket::QosNull {
        info.bt_null = true;
    }
    info
}

/// Upstream `rtw_tx_write_data_rsvd_page_get` (as `RSVD_BEACON`): the packet
/// [`crate::hci::Hci::write_data_rsvd_page`] takes, for a reserved page or a
/// firmware chunk. `band` is the current band (2.4 GHz before any channel).
pub fn rsvd_page_pkt(payload: &[u8], band: Band) -> Vec<u8> {
    build_pkt(
        &rsvd_page_pkt_info(payload, RsvdPacket::Beacon, band),
        payload,
    )
}

/// Upstream `rtw_tx_write_data_h2c_get` and the bus's queue select: only the
/// size and [`QSEL_H2C`] (no offset; the firmware reads the H2C itself).
pub fn h2c_pkt_info(size: usize) -> TxPktInfo {
    TxPktInfo {
        tx_pkt_size: size as u32,
        qsel: QSEL_H2C,
        ..TxPktInfo::default()
    }
}

/// The packet [`crate::hci::Hci::write_data_h2c`] takes.
pub fn h2c_pkt(payload: &[u8]) -> Vec<u8> {
    build_pkt(&h2c_pkt_info(payload.len()), payload)
}
