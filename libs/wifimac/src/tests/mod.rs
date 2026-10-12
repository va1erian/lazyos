//! Host tests against [`FakeRadio`]: a radio that records what it was told
//! and hands back frames the test queued, decrypted as a chip would.

use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use ieee80211::build::{self, BeaconFields};
use ieee80211::ccmp;
use ieee80211::data::{encap, Direction, Encap};
use ieee80211::ie::IeBuilder;
use ieee80211::Mac;

use crate::radio::*;
use crate::{Caller, JoinRequest, KeyRequest, SoftMac};

mod control;
mod data;
mod events;
mod guard;
mod scan;

/// The station manager: `_wlan`, unlabelled, outside any session.
pub(crate) const OWNER: Caller = Caller {
    task: 7,
    uid: 912,
    label_id: 0,
    session: 0,
};
pub(crate) const OTHER: Caller = Caller { task: 9, ..OWNER };
pub(crate) const STA: Mac = [0x02, 0x5A, 0x00, 0x00, 0x00, 0x01];
pub(crate) const AP: Mac = [0x02, 0xA0, 0x00, 0x00, 0x00, 0x01];
pub(crate) const AP2: Mac = [0x02, 0xA0, 0x00, 0x00, 0x00, 0x02];
pub(crate) const GW: Mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0xFE];
pub(crate) const TK: [u8; 16] = [0x11; 16];

#[derive(Default)]
pub(crate) struct FakeRadio {
    pub(crate) tuned: Vec<Channel>,
    pub(crate) sent: Vec<(Vec<u8>, TxOptions)>,
    pub(crate) keys: Vec<(KeyKind, u8, Vec<u8>)>,
    pub(crate) inbound: VecDeque<Received>,
    pub(crate) bssid: Option<(Mac, u16)>,
    pub(crate) scanning: bool,
    pub(crate) state: Option<StaState>,
    pub(crate) address: Option<Mac>,
    pub(crate) queue_full: bool,
    pub(crate) slots_full: bool,
}

impl Radio for FakeRadio {
    fn info(&self) -> RadioInfo {
        RadioInfo {
            mac: STA,
            bands: Band::Ghz2.bit() | Band::Ghz5.bit(),
            ciphers: 1 << CIPHER_CCMP128,
            max_frame: 1484,
            key_slots: 8,
        }
    }
    fn set_address(&mut self, mac: Mac) -> Result<(), RadioError> {
        self.address = Some(mac);
        Ok(())
    }
    fn tune(&mut self, channel: Channel) -> Result<(), RadioError> {
        self.tuned.push(channel);
        Ok(())
    }
    fn set_bssid(&mut self, bssid: Option<Mac>, aid: u16) -> Result<(), RadioError> {
        self.bssid = bssid.map(|b| (b, aid));
        Ok(())
    }
    fn set_scan_filter(&mut self, scanning: bool) -> Result<(), RadioError> {
        self.scanning = scanning;
        Ok(())
    }
    fn set_state(&mut self, state: StaState) -> Result<(), RadioError> {
        self.state = Some(state);
        Ok(())
    }
    fn set_key(&mut self, key: &KeyInstall<'_>) -> Result<(), RadioError> {
        if self.slots_full {
            return Err(RadioError::Full);
        }
        self.keys
            .retain(|(k, i, _)| (*k, *i) != (key.kind, key.index));
        self.keys.push((key.kind, key.index, key.key.to_vec()));
        Ok(())
    }
    fn del_key(&mut self, kind: KeyKind, index: u8) -> Result<(), RadioError> {
        self.keys.retain(|(k, i, _)| (*k, *i) != (kind, index));
        Ok(())
    }
    fn transmit(&mut self, frame: &[u8], options: &TxOptions) -> Result<(), RadioError> {
        if self.queue_full {
            return Err(RadioError::Full);
        }
        self.sent.push((frame.to_vec(), *options));
        Ok(())
    }
    fn tx_ready(&self) -> bool {
        !self.queue_full
    }
    fn receive(&mut self) -> Option<Received> {
        self.inbound.pop_front()
    }
}

pub(crate) fn softmac() -> SoftMac<FakeRadio> {
    SoftMac::new(FakeRadio::default())
}

/// A soft-MAC attached by [`OWNER`] with country `FR`.
pub(crate) fn attached() -> SoftMac<FakeRadio> {
    let mut m = softmac();
    m.attach(OWNER).unwrap();
    m.set_country(OWNER, "FR").unwrap();
    m
}

/// A beacon (or probe response with `probe`) from `bssid` on `channel`,
/// with HT capabilities when `ht`.
pub(crate) fn beacon(bssid: &Mac, ssid: &[u8], channel: u8, ht: bool) -> Vec<u8> {
    let mut ies = IeBuilder::new()
        .ssid(ssid)
        .unwrap()
        .rates(&[0x82, 0x84])
        .unwrap()
        .ds_channel(channel)
        .unwrap();
    if ht {
        ies = ies.raw(45, &[0; 26]).unwrap();
    }
    let fields = BeaconFields {
        timestamp: 1,
        interval: 100,
        capability: 0x0001,
    };
    build::beacon(bssid, 1, fields, &ies.finish())
}

pub(crate) fn probe_response(bssid: &Mac, ssid: &[u8], channel: u8) -> Vec<u8> {
    let ies = IeBuilder::new()
        .ssid(ssid)
        .unwrap()
        .ds_channel(channel)
        .unwrap()
        .finish();
    let fields = BeaconFields {
        timestamp: 1,
        interval: 100,
        capability: 0x0001,
    };
    build::probe_response(bssid, &STA, 1, fields, &ies)
}

/// Queue `frame` as heard on 2.4 GHz `channel`.
pub(crate) fn hear(m: &mut SoftMac<FakeRadio>, frame: Vec<u8>, channel: u8) {
    m.radio_mut().inbound.push_back(Received {
        frame,
        rssi_dbm: -40,
        band: Band::Ghz2,
        channel,
        decrypted: false,
        trailer: 0,
    });
}

/// Join [`AP`] on channel 6 after hearing its beacon in a scan.
pub(crate) fn joined(ht: bool) -> SoftMac<FakeRadio> {
    let mut m = attached();
    let id = m
        .scan(
            OWNER,
            &crate::ScanRequest {
                channels: vec![6],
                band: 0,
                ..Default::default()
            },
            0,
        )
        .unwrap();
    hear(&mut m, beacon(&AP, b"LazyNet", 6, ht), 6);
    m.poll(10);
    m.abort_scan(OWNER, id, 10).unwrap();
    m.join(
        OWNER,
        &JoinRequest {
            bssid: AP.to_vec(),
            channel: 6,
            band: 0,
            width: 0,
        },
        20,
    )
    .unwrap();
    while m.next_event().is_some() {}
    m.radio_mut().sent.clear();
    m
}

/// Associated (AID 1) and authorized on an open network.
pub(crate) fn open_authorized() -> SoftMac<FakeRadio> {
    let mut m = joined(true);
    m.set_state(OWNER, StaState::Associated as u32, 1).unwrap();
    m.set_state(OWNER, StaState::Authorized as u32, 1).unwrap();
    m
}

pub(crate) fn key(kind: KeyKind, index: u32, rsc: &[u8]) -> KeyRequest<'_> {
    KeyRequest {
        kind: kind as u32,
        index,
        cipher: CIPHER_CCMP128,
        key: &TK,
        rsc,
        addr: &AP,
    }
}

/// Associated with a pairwise key (id 0) and group key 1, authorized.
pub(crate) fn wpa2_authorized() -> SoftMac<FakeRadio> {
    let mut m = joined(true);
    m.set_state(OWNER, StaState::Associated as u32, 1).unwrap();
    m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])).unwrap();
    m.set_key(OWNER, &key(KeyKind::Group, 1, &[5])).unwrap();
    m.set_state(OWNER, StaState::Authorized as u32, 1).unwrap();
    m
}

pub(crate) fn ethernet(dst: &Mac, src: &Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut eth = Vec::new();
    eth.extend_from_slice(dst);
    eth.extend_from_slice(src);
    eth.extend_from_slice(&ethertype.to_be_bytes());
    eth.extend_from_slice(payload);
    eth
}

/// A From-DS frame from [`AP`] carrying `eth`. With `pn`, it is protected
/// with key id `key_id` and arrives as a radio that decrypted it and kept
/// the 8-octet MIC delivers it.
pub(crate) fn from_ap(eth: &[u8], tid: Option<u8>, pn: Option<u64>, key_id: u8) -> Received {
    let how = Encap {
        direction: Direction::FromDs,
        bssid: AP,
        tid,
        sequence: 1,
        security: pn.map(|pn| ccmp::header(pn, key_id)),
    };
    let mut frame = encap(eth, &how).unwrap();
    if pn.is_some() {
        frame.extend_from_slice(&[0xEE; 8]);
    }
    Received {
        frame,
        rssi_dbm: -50,
        band: Band::Ghz2,
        channel: 6,
        decrypted: pn.is_some(),
        trailer: if pn.is_some() { 8 } else { 0 },
    }
}

pub(crate) fn receive(m: &mut SoftMac<FakeRadio>, rx: Received, now: u64) {
    m.radio_mut().inbound.push_back(rx);
    m.poll(now);
}
