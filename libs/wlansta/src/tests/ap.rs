//! A test access point: beacons and responses from `ieee80211::build`, and
//! the authenticator side of the 4-way and group handshakes built octet by
//! octet from `lazyos_crypto::wifi` (no `eapol` code path), so a symmetric
//! mistake in the station or the supplicant cannot pass.

use std::vec::Vec;

use ieee80211::build::{self, BeaconFields};
use ieee80211::ie::IeBuilder;
use ieee80211::{cap, Akm, Rsn};
use lazyos_crypto::wifi;

use crate::types::{Mac, ScanEntry};

pub const STA: Mac = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
pub const AP: Mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
pub const AP2: Mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x66];

pub struct Ap {
    pub bssid: Mac,
    pub ssid: Vec<u8>,
    pub channel: u8,
    pub rsn: Option<Rsn>,
    pub rssi: i32,
    pub hidden: bool,
    // The authenticator.
    pub pmk: [u8; 32],
    pub anonce: [u8; 32],
    pub replay: u64,
    pub ptk: [u8; 48],
    pub snonce: Option<[u8; 32]>,
}

impl Ap {
    pub fn open(bssid: Mac, ssid: &str) -> Ap {
        Ap {
            bssid,
            ssid: ssid.as_bytes().to_vec(),
            channel: 6,
            rsn: None,
            rssi: -50,
            hidden: false,
            pmk: [0; 32],
            anonce: [0x33; 32],
            replay: 0,
            ptk: [0; 48],
            snonce: None,
        }
    }

    pub fn wpa2(bssid: Mac, ssid: &str, passphrase: &str, akm: Akm) -> Ap {
        let rsn = match akm {
            Akm::PskSha256 => Rsn::wpa2_psk_sha256(),
            _ => Rsn::wpa2_psk(),
        };
        Ap {
            rsn: Some(rsn),
            pmk: wifi::pbkdf2_sha1(passphrase.as_bytes(), ssid.as_bytes()).unwrap(),
            ..Ap::open(bssid, ssid)
        }
    }

    pub fn akm(&self) -> Akm {
        self.rsn.as_ref().map(|r| r.akms[0]).unwrap_or(Akm::Psk)
    }

    pub fn rsn_ie(&self) -> Vec<u8> {
        self.rsn.as_ref().unwrap().to_ie().unwrap()
    }

    pub fn beacon(&self) -> Vec<u8> {
        let name: &[u8] = if self.hidden { &[] } else { &self.ssid };
        let mut ies = IeBuilder::new()
            .ssid(name)
            .unwrap()
            .rates(&[0x82, 0x84, 0x8B, 0x96, 12, 18, 24, 36])
            .unwrap()
            .ds_channel(self.channel)
            .unwrap();
        let mut capability = cap::ESS;
        if let Some(rsn) = &self.rsn {
            ies = ies.rsn(rsn).unwrap();
            capability |= cap::PRIVACY;
        }
        let fields = BeaconFields {
            timestamp: 1,
            interval: 100,
            capability,
        };
        build::beacon(&self.bssid, 1, fields, &ies.finish())
    }

    pub fn entry(&self) -> ScanEntry {
        ScanEntry {
            frame: self.beacon(),
            channel: u32::from(self.channel),
            band: 0,
            rssi_dbm: self.rssi,
            age_ms: 0,
        }
    }

    pub fn auth_response(&self, status: u16) -> Vec<u8> {
        build::auth(&STA, &self.bssid, &self.bssid, 2, (0, 2, status), &[])
    }

    pub fn assoc_response(&self, status: u16, aid: u16) -> Vec<u8> {
        build::assoc_response(&self.bssid, &STA, 3, cap::ESS, status, aid, &[])
    }

    pub fn deauth(&self, reason: u16) -> Vec<u8> {
        build::deauth(&STA, &self.bssid, &self.bssid, 4, reason)
    }

    fn version(&self) -> u8 {
        if self.akm() == Akm::PskSha256 {
            3
        } else {
            2
        }
    }

    fn mic(&self, frame: &[u8]) -> [u8; 16] {
        let kck: [u8; 16] = self.ptk[..16].try_into().unwrap();
        if self.akm() == Akm::PskSha256 {
            wifi::aes_cmac_128(&kck, frame)
        } else {
            wifi::hmac_sha1_128(&kck, frame)
        }
    }

    fn seal(&self, mut frame: Vec<u8>) -> Vec<u8> {
        frame[81..97].fill(0);
        let mic = self.mic(&frame);
        frame[81..97].copy_from_slice(&mic);
        frame
    }

    /// Wrap an EAPOL PDU as the 802.3 frame the radio delivers.
    pub fn ether(&self, pdu: &[u8]) -> Vec<u8> {
        [&STA[..], &self.bssid, &[0x88, 0x8E], pdu].concat()
    }

    pub fn msg1(&mut self) -> Vec<u8> {
        self.replay += 1;
        let pdu = raw(self.version(), 0x0088, self.replay, &self.anonce, [0; 8], &[]);
        self.ether(&pdu)
    }

    /// Check message 2 (an 802.3 frame from the station) as an AP would:
    /// derive the PTK from its SNonce and verify its MIC. False when the MIC
    /// is wrong (a different passphrase).
    pub fn take_msg2(&mut self, frame: &[u8]) -> bool {
        assert_eq!(&frame[..6], &self.bssid[..], "message 2 goes to the AP");
        assert_eq!(&frame[6..12], &STA[..]);
        let pdu = &frame[14..];
        let snonce: [u8; 32] = pdu[17..49].try_into().unwrap();
        let (lo_a, hi_a) = (self.bssid.min(STA), self.bssid.max(STA));
        let (lo_n, hi_n) = (self.anonce.min(snonce), self.anonce.max(snonce));
        let data = [&lo_a[..], &hi_a, &lo_n, &hi_n].concat();
        let kdf = if self.akm() == Akm::PskSha256 {
            wifi::kdf_sha256
        } else {
            wifi::prf_sha1
        };
        kdf(&self.pmk, b"Pairwise key expansion", &data, &mut self.ptk).unwrap();
        assert_eq!(&pdu[99..], &self.rsn_ie()[..], "message 2 carries the association RSN element");
        let mut zeroed = pdu.to_vec();
        zeroed[81..97].fill(0);
        self.snonce = Some(snonce);
        self.mic(&zeroed) == pdu[81..97]
    }

    pub fn msg3(&mut self, gtk: &[u8; 16]) -> Vec<u8> {
        self.replay += 1;
        let kek: [u8; 16] = self.ptk[16..32].try_into().unwrap();
        let mut plain = self.rsn_ie();
        plain.extend_from_slice(&[0xDD, 22, 0x00, 0x0F, 0xAC, 1, 1, 0]);
        plain.extend_from_slice(gtk);
        let wrapped = wifi::aes_wrap(&kek, &pad(plain)).unwrap();
        let pdu = raw(self.version(), 0x13C8, self.replay, &self.anonce, [7; 8], &wrapped);
        self.ether(&self.seal(pdu))
    }

    pub fn take_msg4(&self, frame: &[u8]) {
        let pdu = &frame[14..];
        assert_eq!(u16::from_be_bytes([pdu[5], pdu[6]]), u16::from(self.version()) | 0x0308);
        let mut zeroed = pdu.to_vec();
        zeroed[81..97].fill(0);
        assert_eq!(self.mic(&zeroed), pdu[81..97], "message 4 MIC");
    }

    pub fn group1(&mut self, index: u8, gtk: &[u8; 16]) -> Vec<u8> {
        self.replay += 1;
        let kek: [u8; 16] = self.ptk[16..32].try_into().unwrap();
        let kde = [&[0xDD, 22, 0x00, 0x0F, 0xAC, 1, index & 3, 0][..], gtk].concat();
        let wrapped = wifi::aes_wrap(&kek, &pad(kde)).unwrap();
        let pdu = raw(self.version(), 0x1380, self.replay, &[0x99; 32], [8; 8], &wrapped);
        self.ether(&self.seal(pdu))
    }

    pub fn tk(&self) -> [u8; 16] {
        self.ptk[32..].try_into().unwrap()
    }
}

/// An EAPOL-Key PDU, octet by octet.
fn raw(version: u8, flags: u16, replay: u64, nonce: &[u8; 32], rsc: [u8; 8], data: &[u8]) -> Vec<u8> {
    let mut f = std::vec![0u8; 99];
    f[0] = 2;
    f[1] = 3;
    f[2..4].copy_from_slice(&((95 + data.len()) as u16).to_be_bytes());
    f[4] = 2;
    f[5..7].copy_from_slice(&(u16::from(version) | flags).to_be_bytes());
    f[7..9].copy_from_slice(&16u16.to_be_bytes());
    f[9..17].copy_from_slice(&replay.to_be_bytes());
    f[17..49].copy_from_slice(nonce);
    f[65..73].copy_from_slice(&rsc);
    f[97..99].copy_from_slice(&(data.len() as u16).to_be_bytes());
    f.extend_from_slice(data);
    f
}

fn pad(mut p: Vec<u8>) -> Vec<u8> {
    if !p.len().is_multiple_of(8) || p.len() < 16 {
        p.push(0xDD);
        while !p.len().is_multiple_of(8) || p.len() < 16 {
            p.push(0);
        }
    }
    p
}
