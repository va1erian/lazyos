//! Building TX packets (upstream `tx.c`: `rtw_tx_pkt_info_update`,
//! `rtw_tx_mgmt_pkt_info_update`, `rtw_tx_data_pkt_info_update`,
//! `rtw_tx_queue_mapping`; `pci.c` `rtw_pci_get_tx_qsel`).
//!
//! The caller hands a complete 802.11 frame (header, CCMP header when
//! protected, body; no FCS: the chip appends it, and the MIC for a CAM key);
//! the result is the 48-byte descriptor followed by the frame, ready for
//! [`Hci::tx_write`] on the queue it names.
//!
//! contract: rtw88-pci owns queue mapping and rate selection in
//! `crate::tx`; this module follows the same upstream functions for the
//! station's two frame kinds and should call `crate::tx` at merge if the
//! two disagree.

use alloc::vec;
use alloc::vec::Vec;

use crate::dev::{Band, Dev, RF_2T2R};
use crate::error::{Error, Result};
use crate::fw::rsvd::lowest_rate;
use crate::fw::txdesc::{self, QSEL_HIGH, QSEL_MGMT};
use crate::hci::{Hci, TxQueue};
use crate::tx::{TxPktInfo, TX_PKT_DESC_SZ};

/// A packet ready for the bus: descriptor + frame, and its queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxFrame {
    pub queue: TxQueue,
    pub pkt: Vec<u8>,
}

/// `DESC_RATE*` used here.
mod desc_rate {
    pub const R1M: u8 = 0x00;
    pub const R11M: u8 = 0x03;
    pub const R6M: u8 = 0x04;
    pub const R54M: u8 = 0x0b;
    pub const MCS7: u8 = 0x13;
    pub const MCS15: u8 = 0x1b;
    pub const VHT1SS_MCS7: u8 = 0x33;
    pub const VHT2SS_MCS7: u8 = 0x3d;
}

/// Frame kinds by the Frame Control field (IEEE 802.11-2020 9.2.4.1).
fn frame_type(frame: &[u8]) -> (u8, u8) {
    let fc0 = frame[0];
    ((fc0 >> 2) & 0x3, (fc0 >> 4) & 0xf)
}

fn is_bmc(frame: &[u8]) -> bool {
    frame[4] & 1 != 0
}

/// 802.1D user priority to access category queue (upstream
/// `ac_to_hwq` over mac80211's TID to AC map).
fn tid_to_queue(tid: u8) -> TxQueue {
    match tid & 7 {
        1 | 2 => TxQueue::Bk,
        0 | 3 => TxQueue::Be,
        4 | 5 => TxQueue::Vi,
        _ => TxQueue::Vo,
    }
}

fn finish(info: &TxPktInfo, frame: &[u8], queue: TxQueue) -> TxFrame {
    let mut pkt = vec![0u8; TX_PKT_DESC_SZ + frame.len()];
    txdesc::fill_tx_desc(info, &mut pkt[..TX_PKT_DESC_SZ]);
    pkt[TX_PKT_DESC_SZ..].copy_from_slice(frame);
    TxFrame { queue, pkt }
}

impl<H: Hci> Dev<H> {
    /// The fields every frame shares (the tail of `rtw_tx_pkt_info_update`).
    fn common_info(&self, frame: &[u8]) -> Result<TxPktInfo> {
        if frame.len() < 10 {
            return Err(Error::Invalid("frame shorter than its addresses"));
        }
        if frame.len() > 0xFFFF {
            return Err(Error::Invalid("frame too long"));
        }
        Ok(TxPktInfo {
            mac_id: self.hal.vif.mac_id,
            bmc: is_bmc(frame),
            tx_pkt_size: frame.len() as u32,
            offset: TX_PKT_DESC_SZ as u8,
            ls: true,
            ..TxPktInfo::default()
        })
    }

    /// Upstream `rtw_tx_mgmt_pkt_info_update`: the lowest basic rate, no
    /// fallback, sequence numbers from the hardware.
    fn mgmt_info(&self, info: &mut TxPktInfo) {
        lowest_rate(info, self.hal.current_band_type);
        // rtw_get_mgmt_rate: the lowest basic rate when the BSS has them.
        let basic = self.hal.vif.basic_rates;
        if basic != 0 {
            let lowest = if self.hal.current_band_type == Band::Ghz2 { desc_rate::R1M } else { desc_rate::R6M };
            info.rate = lowest + basic.trailing_zeros() as u8;
        }
        info.dis_qselseq = true;
        info.en_hwseq = true;
        info.hw_ssn_sel = 0;
    }

    /// A management or control frame (and Null data, which upstream treats
    /// like management): authentication, association, probe requests,
    /// deauthentication, action frames. Never hardware-encrypted.
    pub fn tx_mgmt(&self, frame: &[u8]) -> Result<TxFrame> {
        let mut info = self.common_info(frame)?;
        self.mgmt_info(&mut info);
        let (ty, sub) = frame_type(frame);
        // A QoS Null carries its user priority in the QoS Control field.
        let tid = if ty == 2 && sub == 12 { frame.get(24).map_or(0, |b| b & 7) } else { 0 };
        let (queue, qsel) = match ty {
            0 if sub == 8 => return Err(Error::Invalid("a station sends no beacons")),
            0 | 1 => (TxQueue::Mgmt, QSEL_MGMT),
            2 if sub == 4 || sub == 12 => (tid_to_queue(tid), tid),
            _ => return Err(Error::Invalid("not a management frame")),
        };
        info.qsel = qsel;
        Ok(finish(&info, frame, queue))
    }

    /// A data frame to the AP with user priority `tid`. `hw_key` is the CAM
    /// entry the chip encrypts with ([`Dev::set_key`]'s result); the frame
    /// must then already carry its CCMP header and the Protected bit.
    pub fn tx_data(&self, frame: &[u8], tid: u8, hw_key: Option<u8>) -> Result<TxFrame> {
        if frame.len() < 24 {
            return Err(Error::Invalid("data frame shorter than its header"));
        }
        let mut info = self.common_info(frame)?;
        if frame_type(frame).0 != 2 {
            return Err(Error::Invalid("not a data frame"));
        }
        info.seq = u16::from_le_bytes([frame[22], frame[23]]) >> 4;
        info.rate = desc_rate::R6M;
        info.rate_id = 6;
        if let (false, Some(si)) = (info.bmc, self.hal.vif.sta.as_ref()) {
            info.rate = self.highest_tx_rate(si);
            info.bw = si.bw_mode as u8;
            info.rate_id = si.rate_id;
            info.stbc = !self.hal.txrx_1ss && si.stbc_en != 0;
            info.ldpc = si.ldpc_en != 0;
        }
        if let Some(k) = hw_key {
            let ty = self.key_type(k).ok_or(Error::Invalid("no such key"))?;
            info.sec_type = ty.tx_sec_type();
        }
        let (queue, qsel) = if info.bmc { (TxQueue::Hi0, QSEL_HIGH) } else { (tid_to_queue(tid), tid & 7) };
        info.qsel = qsel;
        Ok(finish(&info, frame, queue))
    }

    /// Upstream `get_highest_vht_tx_rate`/`get_highest_ht_tx_rate`: the
    /// first rate the firmware's adaptation starts from.
    fn highest_tx_rate(&self, si: &crate::station::StaInfo) -> u8 {
        let peer = &si.peer;
        if let Some(vht) = &peer.vht {
            let nss = self.efuse.hw_cap.nss;
            let (map, base) = if nss >= 2 {
                ((vht.tx_mcs_map >> 2) & 0x3, desc_rate::VHT2SS_MCS7)
            } else {
                (vht.tx_mcs_map & 0x3, desc_rate::VHT1SS_MCS7)
            };
            // 0: MCS 0-7, 1: 0-8, anything else: 0-9 (upstream's default).
            return base + map.min(2) as u8;
        }
        if let Some(ht) = &peer.ht {
            return if self.hal.rf_type == RF_2T2R && ht.rx_mask[1] != 0 { desc_rate::MCS15 } else { desc_rate::MCS7 };
        }
        if peer.supp_rates <= 0xf {
            desc_rate::R11M
        } else {
            desc_rate::R54M
        }
    }

    /// Hand `f` to the bus and ring its doorbell (upstream `rtw_tx`).
    pub fn send(&mut self, f: &TxFrame) -> Result<()> {
        self.hci.tx_write(f.queue, &f.pkt)?;
        self.hci.tx_kick_off();
        Ok(())
    }
}
