//! Building TX packets for the station: what upstream's `rtw_tx` does
//! with mac80211's `ieee80211_tx_info` and `rtw_sta_info`, expressed as
//! [`tx::TxParams`] for [`tx::pkt_info_update`] (the PCIe package's
//! translation of `rtw_tx_pkt_info_update`, `rtw_tx_queue_mapping` and
//! `rtw_pci_get_tx_qsel`).
//!
//! The caller hands a complete 802.11 frame (header, CCMP header when
//! protected, body; no FCS: the chip appends it, and the MIC for a CAM key);
//! the result is the 48-byte descriptor followed by the frame, ready for
//! [`Hci::tx_write`] on the queue it names.

use alloc::vec::Vec;

use crate::dev::{Dev, RF_2T2R};
use crate::error::{Error, Result};
use crate::hci::{Hci, TxQueue};
use crate::station::StaInfo;
use crate::tx::{self, Ac, StaTx, TxParams};

/// A packet ready for the bus: descriptor + frame, and its queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxFrame {
    pub queue: TxQueue,
    pub pkt: Vec<u8>,
}

/// Frame type and subtype (IEEE 802.11-2020 9.2.4.1).
fn frame_type(frame: &[u8]) -> Result<(u8, u8)> {
    let fc0 = *frame.first().ok_or(Error::Invalid("empty frame"))?;
    Ok(((fc0 >> 2) & 0x3, (fc0 >> 4) & 0xf))
}

impl<H: Hci> Dev<H> {
    /// The parameters every frame of this interface shares.
    fn tx_params(&self, tid: u8) -> TxParams {
        TxParams {
            mac_id: self.hal.vif.mac_id,
            band: self.hal.current_band_type,
            basic_rates: self.hal.vif.basic_rates,
            ac: Ac::from_tid(tid),
            tid,
            ..TxParams::default()
        }
    }

    fn build(&self, frame: &[u8], p: &TxParams) -> Result<TxFrame> {
        if frame.len() > 0xFFFF {
            return Err(Error::Invalid("frame too long"));
        }
        let (queue, info) = tx::pkt_info_update(frame, p)?;
        Ok(TxFrame {
            queue,
            pkt: tx::build_pkt(&info, frame),
        })
    }

    /// A management or control frame (and Null data, which upstream treats
    /// like management): authentication, association, probe requests,
    /// deauthentication, action frames. Sent at the lowest basic rate,
    /// never hardware-encrypted (upstream's `SW_MGMT_TX` for CCMP).
    pub fn tx_mgmt(&self, frame: &[u8]) -> Result<TxFrame> {
        let (ty, sub) = frame_type(frame)?;
        let tid = match (ty, sub) {
            (0, 8) => return Err(Error::Invalid("a station sends no beacons")),
            (0 | 1, _) => 0,
            // A QoS Null carries its user priority in the QoS Control field.
            (2, 12) => frame.get(24).map_or(0, |b| b & 7),
            (2, 4) => 0,
            _ => return Err(Error::Invalid("not a management frame")),
        };
        self.build(frame, &self.tx_params(tid))
    }

    /// A data frame with user priority `tid`. `hw_key` is the CAM entry the
    /// chip encrypts with ([`Dev::set_key`]'s result); the frame must then
    /// already carry its CCMP header and the Protected bit.
    pub fn tx_data(&self, frame: &[u8], tid: u8, hw_key: Option<u8>) -> Result<TxFrame> {
        if frame_type(frame)?.0 != 2 {
            return Err(Error::Invalid("not a data frame"));
        }
        let mut p = self.tx_params(tid & 7);
        if let Some(k) = hw_key {
            p.sec_type = self
                .key_type(k)
                .ok_or(Error::Invalid("no such key"))?
                .tx_sec_type();
        }
        // Group frames use upstream's defaults; unicast goes to the AP.
        let group = frame.get(4).is_some_and(|b| b & 1 != 0);
        if !group {
            p.sta = self.hal.vif.sta.as_ref().map(|si| self.sta_tx(si));
        }
        self.build(frame, &p)
    }

    /// What upstream's `rtw_tx_data_pkt_info_update` reads from the AP's
    /// `rtw_sta_info`: the rate the firmware's adaptation starts from
    /// (`get_highest_vht_tx_rate`/`get_highest_ht_tx_rate`), its rate id,
    /// width, STBC and LDPC. No A-MPDU and no RTS: this driver does not
    /// aggregate and keeps the default RTS threshold (off).
    fn sta_tx(&self, si: &StaInfo) -> StaTx {
        let peer = &si.peer;
        let rate = if let Some(vht) = &peer.vht {
            tx::info::highest_vht_tx_rate(self.efuse.hw_cap.nss, vht.tx_mcs_map)
        } else if let Some(ht) = &peer.ht {
            tx::info::highest_ht_tx_rate(self.hal.rf_type == RF_2T2R, ht.rx_mask[1])
        } else {
            tx::info::legacy_tx_rate(peer.supp_rates)
        };
        StaTx {
            rate,
            rate_id: si.rate_id,
            bw: si.bw_mode as u8,
            stbc: !self.hal.txrx_1ss && si.stbc_en != 0,
            ldpc: si.ldpc_en != 0,
            rts: false,
            ampdu: None,
        }
    }

    /// Hand `f` to the bus and ring its doorbell (upstream `rtw_tx`).
    pub fn send(&mut self, f: &TxFrame) -> Result<()> {
        self.hci.tx_write(f.queue, &f.pkt)?;
        self.hci.tx_kick_off();
        Ok(())
    }
}
