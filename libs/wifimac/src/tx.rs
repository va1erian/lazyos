//! Transmit: probe requests and deauthentications the soft-MAC builds,
//! management and EAPOL frames from the owner (`TxMgmt`), and Ethernet
//! frames from the network stack. Every data frame gets its sequence number
//! here and, once a pairwise key exists, a CCMP header with the next packet
//! number; the radio encrypts.

use alloc::vec::Vec;

use ieee80211::build;
use ieee80211::ccmp;
use ieee80211::class::{classify, FrameClass};
use ieee80211::data::{encap, is_eapol, user_priority, Direction, Encap, ETH_HEADER};
use ieee80211::frame::{
    SUB_ASSOC_REQ, SUB_AUTH, SUB_DEAUTH, SUB_DISASSOC, SUB_PROBE_REQ,
};
use ieee80211::ie::IeBuilder;
use ieee80211::Mac;

use crate::events::FrameKind;
use crate::mac::SoftMac;
use crate::radio::{Band, Channel, Radio, RadioError, TxOptions};
use crate::Error;

/// Management subtypes a station may send through `TxMgmt`: association and
/// reassociation requests, probe requests, disassociation, authentication,
/// deauthentication, action. Never a beacon or a response.
const SUB_REASSOC_REQ: u8 = 2;
const SUB_ACTION: u8 = 13;
const STATION_MGMT: [u8; 7] = [
    SUB_ASSOC_REQ,
    SUB_REASSOC_REQ,
    SUB_PROBE_REQ,
    SUB_DISASSOC,
    SUB_AUTH,
    SUB_DEAUTH,
    SUB_ACTION,
];

/// The smallest EAPOL frame: Ethernet header and the 4-octet EAPOL header.
const EAPOL_MIN: usize = ETH_HEADER + 4;

/// The basic rates a probe request offers: 1, 2, 5.5 and 11 Mb/s on 2.4 GHz,
/// 6, 12 and 24 Mb/s (basic) plus 9, 18, 36, 48, 54 elsewhere.
const RATES_2GHZ: [u8; 8] = [0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24];
const RATES_OFDM: [u8; 8] = [0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C];

/// Why an Ethernet frame from the network stack was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataDrop {
    /// The port is closed (not authorized, or a protected BSS without its
    /// pairwise key).
    PortClosed,
    /// Under 14 octets.
    Runt,
    /// Longer than the MTU allows.
    TooLong,
    /// EAPOL (only the owner sends it, through `TxMgmt`) or a source address
    /// that is not the station's.
    Refused,
    /// The radio's queue is full.
    Full,
}

const MGMT_OPTIONS: TxOptions = TxOptions {
    data: false,
    protected: false,
    basic_rate: true,
    priority: 7,
};

impl<R: Radio> SoftMac<R> {
    /// Queue `frame` on the radio, counting a hardware failure.
    fn transmit(&mut self, frame: &[u8], options: &TxOptions) -> Result<(), Error> {
        self.radio
            .transmit(frame, options)
            .map_err(|e| match e {
                RadioError::Full => Error::Busy,
                other => self.radio_failed(other),
            })
    }

    /// A management frame the soft-MAC built: sequence it and send it.
    fn send_own_mgmt(&mut self, mut frame: Vec<u8>) {
        let seq = self.seq.next(None);
        frame[22..24].copy_from_slice(&(seq << 4).to_le_bytes());
        if self.transmit(&frame, &MGMT_OPTIONS).is_ok() {
            self.stats.mgmt_tx += 1;
        }
    }

    /// Probe requests on the channel just tuned: one per SSID asked for, or
    /// one wildcard.
    pub(crate) fn send_probes(&mut self, channel: Channel, ssids: &[Vec<u8>]) {
        let wildcard: [&[u8]; 1] = [&[]];
        let names: Vec<&[u8]> = if ssids.is_empty() {
            wildcard.to_vec()
        } else {
            ssids.iter().map(Vec::as_slice).collect()
        };
        let rates: &[u8] = if channel.band == Band::Ghz2 {
            &RATES_2GHZ
        } else {
            &RATES_OFDM
        };
        for ssid in names {
            let ies = IeBuilder::new()
                .ssid(ssid)
                .and_then(|ies| ies.rates(rates))
                .and_then(|ies| match channel.band {
                    Band::Ghz2 => ies.ds_channel(channel.number),
                    _ => Ok(ies),
                });
            // The SSIDs were length-checked when the scan was accepted.
            let Ok(ies) = ies else { continue };
            let frame = build::probe_request(&self.info.mac, 0, &ies.finish());
            self.send_own_mgmt(frame);
        }
    }

    /// A deauthentication to the AP (the radio cannot protect management
    /// frames, so it goes in the clear).
    pub(crate) fn send_deauth(&mut self, bssid: &Mac, reason: u16) {
        let frame = build::deauth(bssid, &self.info.mac, bssid, 0, reason);
        self.send_own_mgmt(frame);
    }

    /// `TxMgmt`: a management frame or an EAPOL frame the owner built.
    pub fn tx_mgmt(&mut self, caller: u64, kind: u32, frame: &[u8]) -> Result<(), Error> {
        self.owned(caller)?;
        let kind = FrameKind::from_u32(kind).ok_or(Error::Invalid)?;
        if self.domain.is_none() {
            return Err(Error::Again);
        }
        if self.bss.is_none() {
            return Err(Error::NotConnected);
        }
        if frame.len() > self.max_mgmt() {
            self.stats.mgmt_dropped += 1;
            return Err(Error::Invalid);
        }
        match kind {
            FrameKind::Mgmt => self.tx_station_mgmt(frame)?,
            FrameKind::Eapol => self.tx_eapol(frame)?,
        }
        self.stats.mgmt_tx += 1;
        Ok(())
    }

    /// A station management frame: an allowed subtype, no flags (so no
    /// Protected bit and no HT Control), from our own address. The soft-MAC
    /// assigns the sequence number and clears the duration.
    fn tx_station_mgmt(&mut self, frame: &[u8]) -> Result<(), Error> {
        let FrameClass::Mgmt { subtype, .. } = classify(frame) else {
            return Err(Error::Invalid);
        };
        if !STATION_MGMT.contains(&subtype) || frame[1] != 0 || frame[10..16] != self.info.mac {
            return Err(Error::Invalid);
        }
        let mut out = Vec::from(frame);
        out[2..4].copy_from_slice(&[0, 0]);
        let seq = self.seq.next(None);
        out[22..24].copy_from_slice(&(seq << 4).to_le_bytes());
        self.transmit(&out, &MGMT_OPTIONS)
    }

    /// An EAPOL frame from our address, sent as a data frame to the BSS:
    /// in the clear before the pairwise key, protected after it.
    fn tx_eapol(&mut self, eth: &[u8]) -> Result<(), Error> {
        if !is_eapol(eth) || eth.len() < EAPOL_MIN || eth[6..12] != self.info.mac {
            return Err(Error::Invalid);
        }
        let (frame, protected) = self.build_data(eth)?;
        let options = TxOptions {
            data: true,
            protected,
            basic_rate: true,
            priority: 7,
        };
        self.transmit(&frame, &options)
    }

    /// The To-DS data frame carrying `eth`, and whether it is protected.
    fn build_data(&mut self, eth: &[u8]) -> Result<(Vec<u8>, bool), Error> {
        let bss = self.bss.ok_or(Error::NotConnected)?;
        let tid = bss.qos.then(|| user_priority(eth));
        let security = if self.keys.pairwise.is_some() {
            let (pn, key_id) = self.keys.next_tx().ok_or(Error::NoSpace)?;
            Some(ccmp::header(pn, key_id))
        } else {
            None
        };
        let how = Encap {
            direction: Direction::ToDs,
            bssid: bss.bssid,
            tid,
            sequence: self.seq.next(tid),
            security,
        };
        let frame = encap(eth, &how).map_err(|_| Error::Invalid)?;
        Ok((frame, security.is_some()))
    }

    /// An Ethernet frame from the network stack.
    pub fn send_ethernet(&mut self, eth: &[u8]) -> Result<(), DataDrop> {
        let result = self.send_ethernet_inner(eth);
        match result {
            Ok(()) => self.stats.tx_data += 1,
            Err(_) => self.stats.tx_data_dropped += 1,
        }
        result
    }

    fn send_ethernet_inner(&mut self, eth: &[u8]) -> Result<(), DataDrop> {
        let protected_bss = self.bss.is_some_and(|bss| bss.protected);
        if !self.link_up() || (protected_bss && self.keys.pairwise.is_none()) {
            return Err(DataDrop::PortClosed);
        }
        if eth.len() < ETH_HEADER {
            return Err(DataDrop::Runt);
        }
        if eth.len() > usize::from(self.mtu()) + ETH_HEADER {
            return Err(DataDrop::TooLong);
        }
        if is_eapol(eth) || eth[6..12] != self.info.mac {
            return Err(DataDrop::Refused);
        }
        let (frame, protected) = self.build_data(eth).map_err(|_| DataDrop::Refused)?;
        if frame.len() > self.info.max_frame {
            return Err(DataDrop::TooLong);
        }
        let options = TxOptions {
            data: true,
            protected,
            basic_rate: false,
            priority: user_priority(eth),
        };
        self.transmit(&frame, &options).map_err(|_| DataDrop::Full)
    }

    /// Whether the radio would take a frame now.
    pub fn tx_ready(&self) -> bool {
        self.radio.tx_ready()
    }
}
