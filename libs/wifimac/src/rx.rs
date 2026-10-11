//! Receive processing: every frame the radio hands over is classified first;
//! beacons and probe responses feed the scan and the beacon watch,
//! management frames from the joined BSS go to the owner as `RxMgmt`, data
//! frames are checked (from our BSS, to us, decrypted, not replayed),
//! decapsulated, and split: EAPOL to the owner, the rest to the network
//! stack once the port is open.

use alloc::vec::Vec;

use ieee80211::ccmp;
use ieee80211::class::{classify, classify_msdu, FrameClass, MsduClass};
use ieee80211::data::{decap, DataHeader};
use ieee80211::frame::{
    SUB_ASSOC_RESP, SUB_AUTH, SUB_BEACON, SUB_DEAUTH, SUB_DISASSOC, SUB_PROBE_RESP,
};
use ieee80211::{is_group, Mac};

use crate::events::{Event, FrameKind};
use crate::mac::{bss_traits, SoftMac, RX_QUEUE};
use crate::radio::{Radio, Received, StaState};

/// Frames taken from the radio per [`SoftMac::poll`].
pub(crate) const RX_BUDGET: usize = 64;

/// Reassociation response and action: the other management frames an AP
/// sends a station that the station manager may want.
const SUB_REASSOC_RESP: u8 = 3;
const SUB_ACTION: u8 = 13;

fn addr(frame: &[u8], at: usize) -> Mac {
    let mut out = [0u8; 6];
    out.copy_from_slice(&frame[at..at + 6]);
    out
}

impl<R: Radio> SoftMac<R> {
    pub(crate) fn on_receive(&mut self, rx: Received, now: u64) {
        match classify(&rx.frame) {
            // No management frame protection: a protected one is unreadable.
            FrameClass::Mgmt { subtype, protected } if !protected => self.on_mgmt(subtype, rx, now),
            FrameClass::Data { protected } => self.on_data(protected, rx),
            _ => {}
        }
    }

    fn on_mgmt(&mut self, subtype: u8, rx: Received, now: u64) {
        // `classify` checked the 24-octet header is there.
        let (a1, a2, a3) = (addr(&rx.frame, 4), addr(&rx.frame, 10), addr(&rx.frame, 16));
        if subtype == SUB_BEACON || subtype == SUB_PROBE_RESP {
            self.scanner
                .record(&rx.frame, rx.band, rx.channel, rx.rssi_dbm, now);
            if let Some(bss) = self.bss.as_mut() {
                let ours = a2 == bss.bssid && a3 == bss.bssid;
                if ours && rx.channel == bss.channel.number && rx.band == bss.channel.band {
                    bss.last_beacon = now;
                    bss.loss_reported = false;
                    if let Some((qos, interval)) = bss_traits(&rx.frame) {
                        bss.qos = qos;
                        bss.interval_ms = interval;
                    }
                }
            }
            return;
        }
        let wanted = [
            SUB_AUTH,
            SUB_ASSOC_RESP,
            SUB_REASSOC_RESP,
            SUB_DEAUTH,
            SUB_DISASSOC,
            SUB_ACTION,
        ];
        let Some(bss) = self.bss else {
            return;
        };
        let ending = subtype == SUB_DEAUTH || subtype == SUB_DISASSOC;
        let to_us = a1 == self.info.mac || (ending && is_group(&a1));
        if !wanted.contains(&subtype) || !to_us || a2 != bss.bssid || a3 != bss.bssid {
            return;
        }
        if rx.frame.len() > self.max_mgmt() {
            self.stats.mgmt_dropped += 1;
            return;
        }
        let reason = match rx.frame.get(24..26) {
            Some(&[lo, hi]) if ending => Some(u16::from_le_bytes([lo, hi])),
            _ => None,
        };
        self.stats.mgmt_rx += 1;
        self.emit(Event::RxMgmt {
            kind: FrameKind::Mgmt,
            rssi_dbm: i32::from(rx.rssi_dbm),
            frame: rx.frame,
        });
        if let Some(reason) = reason {
            self.stats.deauths += 1;
            self.emit(Event::Deauthenticated { reason });
            self.drop_association();
        }
    }

    fn drop_data(&mut self) {
        self.stats.rx_data_dropped += 1;
    }

    /// Where the payload of a received data frame starts, after the replay
    /// check of a protected frame; `None` drops it.
    fn payload_start(
        &mut self,
        header: &DataHeader,
        protected: bool,
        rx: &Received,
    ) -> Option<usize> {
        if !protected {
            // A plaintext frame has nothing for the radio to have kept.
            return (!rx.decrypted && rx.trailer == 0).then_some(header.len);
        }
        if !rx.decrypted {
            return None;
        }
        let (pn, key_id) = ccmp::parse_header(rx.frame.get(header.len..)?).ok()?;
        let window = self.keys.window(is_group(&header.addr1), key_id)?;
        // The radio verified the MIC, so the PN is authentic and may advance
        // the window.
        if !window.accept(header.tid(), pn) {
            self.stats.replays += 1;
            return None;
        }
        Some(header.len + ccmp::HEADER_LEN)
    }

    fn on_data(&mut self, protected: bool, rx: Received) {
        let Some(bss) = self.bss else {
            return self.drop_data();
        };
        let Ok(header) = DataHeader::parse(&rx.frame) else {
            return self.drop_data();
        };
        let from_bss = header.from_ds() && !header.to_ds() && header.addr2 == bss.bssid;
        let to_us = header.addr1 == self.info.mac || is_group(&header.addr1);
        if !from_bss || !to_us {
            return self.drop_data();
        }
        let Some(start) = self.payload_start(&header, protected, &rx) else {
            return self.drop_data();
        };
        let Some(end) = rx
            .frame
            .len()
            .checked_sub(rx.trailer)
            .filter(|&e| e >= start)
        else {
            return self.drop_data();
        };
        let mut frames = Vec::new();
        if decap(&header, &rx.frame[start..end], &mut frames).is_err() {
            return self.drop_data();
        }
        for eth in frames {
            self.deliver(eth, protected, rx.rssi_dbm);
        }
    }

    /// One decapsulated Ethernet frame: EAPOL to the owner, the rest to the
    /// network stack if the port is open.
    fn deliver(&mut self, eth: Vec<u8>, protected: bool, rssi_dbm: i8) {
        // An AP reflects our own group frames back: never pass them up.
        if eth.get(6..12) == Some(&self.info.mac[..]) {
            return self.drop_data();
        }
        if classify_msdu(&eth) == MsduClass::Eapol {
            self.stats.mgmt_rx += 1;
            self.emit(Event::RxMgmt {
                kind: FrameKind::Eapol,
                rssi_dbm: i32::from(rssi_dbm),
                frame: eth,
            });
            return;
        }
        let protected_bss = self.bss.is_some_and(|bss| bss.protected);
        let open = self.state == StaState::Authorized && (protected || !protected_bss);
        if !open || self.rx_eth.len() >= RX_QUEUE {
            return self.drop_data();
        }
        self.stats.rx_data += 1;
        self.rx_eth.push_back(eth);
    }

    /// The next Ethernet frame for the network stack.
    pub fn next_ethernet(&mut self) -> Option<Vec<u8>> {
        self.rx_eth.pop_front()
    }
}
