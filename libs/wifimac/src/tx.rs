//! Transmit: probe requests and deauthentications the soft-MAC builds,
//! management and EAPOL frames from the owner (`TxMgmt`), and Ethernet
//! frames from the network stack. Every data frame gets its sequence number
//! here and, once a pairwise key exists, a CCMP header with the next packet
//! number; the radio encrypts.
//!
//! Nothing goes to the BSS while the radio is tuned elsewhere (a scan step)
//! or, on a channel the domain lists as passive, before the BSS was heard
//! there ([`SoftMac::tx_allowed`]): the owner's frames wait, the network
//! stack's stay in its ring ([`SoftMac::tx_ready`]). Probe requests go out
//! only on the scan steps the domain lets the radio probe on.

use alloc::vec::Vec;

use ieee80211::build;
use ieee80211::ccmp;
use ieee80211::class::{classify, FrameClass};
use ieee80211::data::{encap, is_eapol, user_priority, Direction, Encap, ETH_HEADER};
use ieee80211::frame::{SUB_ASSOC_REQ, SUB_AUTH, SUB_DEAUTH, SUB_DISASSOC, SUB_PROBE_REQ};
use ieee80211::ie::IeBuilder;
use ieee80211::Mac;

use crate::bss::Bss;
use crate::events::FrameKind;
use crate::mac::{SoftMac, PENDING_TX};
use crate::radio::{Band, Channel, Radio, RadioError, TxOptions};
use crate::{Caller, Error};

const BROADCAST: Mac = [0xFF; 6];

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
        self.radio.transmit(frame, options).map_err(|e| match e {
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

    /// `TxMgmt`: a management frame or an EAPOL frame the owner built. It is
    /// sequenced (and, EAPOL under a pairwise key, given its PN) now, under
    /// the keys in force now, and goes out at once or, while the radio is
    /// away on a scan step or may not yet transmit on a passive channel, as
    /// soon as it may (`Busy` when [`PENDING_TX`] frames already wait).
    pub fn tx_mgmt(&mut self, caller: Caller, kind: u32, frame: &[u8]) -> Result<(), Error> {
        self.owned(caller)?;
        let kind = FrameKind::from_u32(kind).ok_or(Error::Invalid)?;
        if self.domain.is_none() {
            return Err(Error::Again);
        }
        let bss = self.bss.ok_or(Error::NotConnected)?;
        if frame.len() > self.max_mgmt() {
            self.stats.mgmt_dropped += 1;
            return Err(Error::Invalid);
        }
        let (out, options) = match kind {
            FrameKind::Mgmt => self.station_mgmt(frame, &bss)?,
            FrameKind::Eapol => self.eapol(frame, &bss)?,
        };
        self.send_or_hold(out, options)?;
        self.stats.mgmt_tx += 1;
        Ok(())
    }

    /// A station management frame: an allowed subtype, no flags (so no
    /// Protected bit and no HT Control), from our own address to the BSS
    /// (receiver and BSSID; a probe request may be broadcast in either).
    /// The soft-MAC assigns the sequence number and clears the duration.
    fn station_mgmt(&mut self, frame: &[u8], bss: &Bss) -> Result<(Vec<u8>, TxOptions), Error> {
        let FrameClass::Mgmt { subtype, .. } = classify(frame) else {
            return Err(Error::Invalid);
        };
        let broadcast_ok = |at: usize| subtype == SUB_PROBE_REQ && frame[at..at + 6] == BROADCAST;
        let a1_ok = frame[4..10] == bss.bssid || broadcast_ok(4);
        let a3_ok = frame[16..22] == bss.bssid || broadcast_ok(16);
        if !STATION_MGMT.contains(&subtype)
            || frame[1] != 0
            || frame[10..16] != self.info.mac
            || !a1_ok
            || !a3_ok
        {
            return Err(Error::Invalid);
        }
        let mut out = Vec::from(frame);
        out[2..4].copy_from_slice(&[0, 0]);
        let seq = self.seq.next(None);
        out[22..24].copy_from_slice(&(seq << 4).to_le_bytes());
        if let Some(bss) = self.bss.as_mut() {
            match subtype {
                SUB_AUTH => bss.sent_auth = true,
                SUB_ASSOC_REQ | SUB_REASSOC_REQ => bss.sent_assoc = true,
                _ => {}
            }
        }
        Ok((out, MGMT_OPTIONS))
    }

    /// An EAPOL frame from our address to the BSS, as a data frame: in the
    /// clear before the pairwise key, protected after it.
    fn eapol(&mut self, eth: &[u8], bss: &Bss) -> Result<(Vec<u8>, TxOptions), Error> {
        let ours = eth.len() >= EAPOL_MIN && eth[6..12] == self.info.mac;
        if !is_eapol(eth) || !ours || eth[0..6] != bss.bssid {
            return Err(Error::Invalid);
        }
        let (frame, protected) = self.build_data(eth)?;
        let options = TxOptions {
            data: true,
            protected,
            basic_rate: true,
            priority: 7,
        };
        Ok((frame, options))
    }

    /// Send an owner frame now, or hold it until the radio may.
    fn send_or_hold(&mut self, frame: Vec<u8>, options: TxOptions) -> Result<(), Error> {
        if self.pending.is_empty() && self.tx_allowed() {
            return self.transmit(&frame, &options);
        }
        if self.pending.len() >= PENDING_TX {
            return Err(Error::Busy);
        }
        self.pending.push_back((frame, options));
        Ok(())
    }

    /// Send the held frames, in order, while the radio may and takes them.
    pub(crate) fn flush_pending(&mut self) {
        while self.tx_allowed() {
            let Some((frame, options)) = self.pending.pop_front() else {
                return;
            };
            if let Err(error) = self.radio.transmit(&frame, &options) {
                if error == RadioError::Full {
                    self.pending.push_front((frame, options));
                } else {
                    self.radio_failed(error);
                }
                return;
            }
        }
    }

    /// The To-DS data frame carrying `eth`, and whether it is protected. A
    /// frame over the radio's largest is refused before it uses a PN.
    fn build_data(&mut self, eth: &[u8]) -> Result<(Vec<u8>, bool), Error> {
        let bss = self.bss.ok_or(Error::NotConnected)?;
        let tid = bss.qos.then(|| user_priority(eth));
        let pn = match self.keys.pairwise {
            Some(_) => Some(self.keys.peek_tx().ok_or(Error::NoSpace)?),
            None => None,
        };
        let how = Encap {
            direction: Direction::ToDs,
            bssid: bss.bssid,
            tid,
            sequence: self.seq.next(tid),
            security: pn.map(|(pn, key_id)| ccmp::header(pn, key_id)),
        };
        let frame = encap(eth, &how).map_err(|_| Error::Invalid)?;
        if frame.len() > self.info.max_frame {
            return Err(Error::Invalid);
        }
        if let Some((pn, _)) = pn {
            self.keys.commit_tx(pn);
        }
        Ok((frame, pn.is_some()))
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
        let protected_bss = self.bss.is_some_and(|bss| bss.protected || bss.privacy);
        if !self.link_up() || (protected_bss && self.keys.pairwise.is_none()) {
            return Err(DataDrop::PortClosed);
        }
        if !self.tx_allowed() {
            // Away on a scan step: `tx_ready` holds the stack's frames back,
            // this is one that came anyway.
            return Err(DataDrop::Full);
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
        let (frame, protected) = self.build_data(eth).map_err(|e| match e {
            Error::Invalid => DataDrop::TooLong,
            _ => DataDrop::Refused,
        })?;
        let options = TxOptions {
            data: true,
            protected,
            basic_rate: false,
            priority: user_priority(eth),
        };
        self.transmit(&frame, &options).map_err(|_| DataDrop::Full)
    }

    /// Whether the radio would take a frame from the network stack now: not
    /// while it is away from the joined BSS's channel.
    pub fn tx_ready(&self) -> bool {
        self.radio.tx_ready() && (self.bss.is_none() || self.tx_allowed())
    }
}
