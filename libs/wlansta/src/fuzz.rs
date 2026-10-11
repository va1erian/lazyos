//! The fuzz entry point: any bytes are a script of radio events and
//! commands against one [`Station`], with the invariants checked after every
//! step. The seeded tests run it under plain `cargo test`.
//!
//! Script grammar: an opcode byte, then the operands it reads (a byte cursor
//! that yields zeros past the end). The access points are fixed: an open
//! "cafe" at `AP` and a WPA2 "home" at `AP2`, so random scripts reach the
//! join states and not only refusals.

use std::vec;
use std::vec::Vec;

use ieee80211::build::{self, BeaconFields};
use ieee80211::ie::IeBuilder;
use ieee80211::{cap, Rsn};

use crate::types::*;
use crate::{Key, Station, Viewer};

const STA: Mac = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
const AP: Mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
const AP2: Mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x66];
const CAFE: &str = "w000000000001";
const HOME: &str = "w000000000002";

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn byte(&mut self) -> u8 {
        match self.0.split_first() {
            Some((&b, rest)) => {
                self.0 = rest;
                b
            }
            None => 0,
        }
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.byte(), self.byte()])
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = usize::from(self.byte()).min(max).min(self.0.len());
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        head.to_vec()
    }

    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

fn profiles(mask: u8) -> Vec<Profile> {
    let mut out = Vec::new();
    for (bit, id, ssid, security) in [
        (1, CAFE, &b"cafe"[..], Security::Open),
        (2, HOME, &b"home"[..], Security::Wpa2Psk),
    ] {
        if mask & bit != 0 {
            out.push(Profile {
                id: id.into(),
                ssid: ssid.to_vec(),
                security,
                hidden: mask & 4 != 0,
                autoconnect: mask & 8 == 0,
                priority: u32::from(mask >> 4),
                owner: Owner::System,
            });
        }
    }
    out
}

fn beacon(bssid: &Mac, ssid: &[u8], wpa2: bool) -> Vec<u8> {
    let mut ies = IeBuilder::new().ssid(ssid).unwrap().ds_channel(6).unwrap();
    let mut capability = cap::ESS;
    if wpa2 {
        ies = ies.rsn(&Rsn::wpa2_psk()).unwrap();
        capability |= cap::PRIVACY;
    }
    let fields = BeaconFields {
        timestamp: 0,
        interval: 100,
        capability,
    };
    build::beacon(bssid, 1, fields, &ies.finish())
}

fn entry(c: &mut Cursor<'_>) -> ScanEntry {
    let frame = match c.byte() % 3 {
        0 => beacon(&AP, b"cafe", false),
        1 => beacon(&AP2, b"home", true),
        _ => c.bytes(255),
    };
    ScanEntry {
        frame,
        channel: u32::from(c.byte() % 16),
        band: u32::from(c.byte() % 4),
        rssi_dbm: -i32::from(c.byte()),
        age_ms: u32::from(c.u16()),
    }
}

fn mgmt(c: &mut Cursor<'_>) -> Vec<u8> {
    let from = if c.byte() & 1 == 0 { AP } else { AP2 };
    let status = u16::from(c.byte() % 3);
    match c.byte() % 4 {
        0 => build::auth(&STA, &from, &from, 2, (0, 2, status), &[]),
        1 => build::assoc_response(&from, &STA, 2, cap::ESS, status, 1, &[]),
        2 => build::deauth(&STA, &from, &from, 2, status),
        _ => c.bytes(255),
    }
}

/// The invariants that hold after every step.
fn check<N: FnMut() -> [u8; 32] + Clone>(sta: &Station<N>, now: u64, ticked: bool, actions: &[Action]) {
    let state = sta.status().state;
    let expected = match sta.phase_name() {
        "idle" => ConnState::Idle,
        "scanning" => ConnState::Scanning,
        "wait-pmk" | "auth" => ConnState::Authenticating,
        "assoc" => ConnState::Associating,
        "handshake" => ConnState::Handshake,
        "connected" => ConnState::Connected,
        _ => ConnState::Failed,
    };
    assert_eq!(state, expected, "status follows the phase");
    if ticked {
        assert!(sta.next_deadline().is_none_or(|at| at > now), "no deadline left overdue");
    }
    for action in actions {
        if let Action::SetKey { .. } = action {
            assert!(matches!(sta.phase_name(), "handshake" | "connected" | "failed"));
        }
    }
    for network in sta.networks(Viewer::User(1000)) {
        assert!(network.ssid.len() <= 32 && !network.ssid.chars().any(char::is_control));
    }
    assert!(sta.status().ssid.len() <= 32);
}

/// Run a script; returns the final state.
pub fn run(data: &[u8]) -> ConnState {
    let mut c = Cursor(data);
    let mut sta = Station::new(STA, (|| [0x5A; 32]) as fn() -> [u8; 32]);
    let mut now = 1_000u64;
    let actions = sta.set_profiles(now, profiles(c.byte()));
    check(&sta, now, false, &actions);
    while !c.done() {
        let mut ticked = false;
        let actions = match c.byte() % 16 {
            0 => {
                now += u64::from(c.u16());
                ticked = true;
                sta.input(now, Event::Tick)
            }
            1 => sta.input(now, Event::ScanStarted { scan_id: u32::from(c.byte() % 4) }),
            2 => {
                let scan_id = u32::from(c.byte() % 4);
                let entries = (0..c.byte() % 4).map(|_| entry(&mut c)).collect();
                sta.input(now, Event::ScanDone { scan_id, aborted: false, entries })
            }
            3..=5 => {
                let frame = mgmt(&mut c);
                sta.input(now, Event::Rx { kind: FrameKind::Mgmt, rssi_dbm: -40, frame })
            }
            6 => {
                let frame = c.bytes(255);
                sta.input(now, Event::Rx { kind: FrameKind::Eapol, rssi_dbm: -40, frame })
            }
            7 => sta.input(now, Event::BeaconLoss),
            8 => sta.input(now, Event::Deauthenticated { reason: c.u16() }),
            9 => {
                let op = [HwOp::Scan, HwOp::AbortScan, HwOp::Join, HwOp::Tx, HwOp::SetState, HwOp::SetKey, HwOp::Leave];
                let op = op[usize::from(c.byte()) % op.len()];
                sta.input(now, Event::HwError { op, errno: -5 })
            }
            10 => {
                let id = if c.byte() & 1 == 0 { CAFE } else { HOME };
                sta.connect(now, id).unwrap_or_default()
            }
            11 => sta.disconnect(now),
            12 => sta.scan(now, vec![]).unwrap_or_default(),
            13 => {
                let answer = match c.byte() % 3 {
                    0 => Ok(Key::new([c.byte(); 32])),
                    1 => Err(PmkError::Missing),
                    _ => Err(PmkError::Unavailable),
                };
                sta.pmk(now, HOME, answer)
            }
            14 => sta.set_profiles(now, profiles(c.byte())),
            _ => sta.set_active_users(now, vec![u32::from(c.byte())]),
        };
        check(&sta, now, ticked, &actions);
    }
    sta.status().state
}

/// A script that joins the open network: profiles, scan id 3 with the cafe
/// beacon, a successful authentication and association.
pub fn open_join_script() -> Vec<u8> {
    vec![
        1, // profiles: cafe only
        1, 3, // ScanStarted 3
        2, 3, 1, 0, 6, 0, 40, 0, 0, // ScanDone 3: the cafe beacon, ch 6, 2.4 GHz
        3, 0, 0, 0, // auth response from AP, success
        3, 0, 0, 1, // association response from AP, success
    ]
}
