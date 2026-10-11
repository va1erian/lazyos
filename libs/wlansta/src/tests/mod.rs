//! Host tests: joins against a test access point, failures and timers,
//! hostile input, the policy and profile rules, and a seeded fuzz soak.

use std::string::String;
use std::vec::Vec;

use crate::types::*;
use crate::{Key, Station};

mod ap;
mod failures;
mod hostile;
mod joins;
mod rules;

use ap::Ap;

pub(crate) const HOME: &str = "w000000000001";
pub(crate) const CAFE: &str = "w000000000002";

fn nonce() -> [u8; 32] {
    [0x77; 32]
}

pub(crate) type Sta = Station<fn() -> [u8; 32]>;

/// A station and its clock.
pub(crate) struct H {
    pub sta: Sta,
    pub now: u64,
    pub next_scan_id: u32,
    /// What setting the profiles asked for (auto-connect starts there).
    pub first: Vec<Action>,
}

pub(crate) fn profile(id: &str, ssid: &str, security: Security) -> Profile {
    Profile {
        id: String::from(id),
        ssid: ssid.as_bytes().to_vec(),
        security,
        hidden: false,
        autoconnect: true,
        priority: 0,
        owner: Owner::System,
    }
}

impl H {
    pub fn new(profiles: Vec<Profile>) -> H {
        let mut sta: Sta = Station::new(ap::STA, nonce as fn() -> [u8; 32]);
        let first = sta.set_profiles(1_000, profiles);
        H {
            sta,
            now: 1_000,
            next_scan_id: 1,
            first,
        }
    }

    /// The actions auto-connect started with.
    pub fn begin(&mut self) -> Vec<Action> {
        let mut actions = core::mem::take(&mut self.first);
        actions.extend(self.sta.start(self.now));
        actions
    }

    pub fn input(&mut self, event: Event) -> Vec<Action> {
        self.sta.input(self.now, event)
    }

    pub fn advance(&mut self, ms: u64) -> Vec<Action> {
        self.now += ms;
        self.sta.input(self.now, Event::Tick)
    }

    pub fn rx(&mut self, kind: FrameKind, frame: Vec<u8>) -> Vec<Action> {
        self.input(Event::Rx {
            kind,
            rssi_dbm: -40,
            frame,
        })
    }

    /// Answer the `Scan` that `actions` must hold with these APs.
    pub fn answer_scan(&mut self, actions: Vec<Action>, aps: &[&Ap]) -> Vec<Action> {
        assert!(
            actions.iter().any(|a| matches!(a, Action::Scan { .. })),
            "expected a scan: {actions:?}"
        );
        let scan_id = self.next_scan_id;
        self.next_scan_id += 1;
        assert!(self.input(Event::ScanStarted { scan_id }).is_empty());
        self.input(Event::ScanDone {
            scan_id,
            aborted: false,
            entries: aps.iter().map(|ap| ap.entry()).collect(),
        })
    }

    pub fn state(&self) -> (ConnState, FailReason) {
        (self.sta.status().state, self.sta.status().reason)
    }

    /// Give the PMK the `FetchPmk` in `actions` asks for.
    pub fn answer_pmk(&mut self, actions: Vec<Action>, pmk: [u8; 32]) -> Vec<Action> {
        let id = match actions.as_slice() {
            [Action::FetchPmk { id, .. }] => id.clone(),
            other => panic!("expected FetchPmk: {other:?}"),
        };
        self.sta.pmk(self.now, &id, Ok(Key::new(pmk)))
    }
}

/// The frames of the `Tx` actions.
pub(crate) fn sent(actions: &[Action]) -> Vec<(FrameKind, Vec<u8>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Tx { kind, frame } => Some((*kind, frame.clone())),
            _ => None,
        })
        .collect()
}

/// The single EAPOL frame sent in `actions`.
pub(crate) fn eapol_sent(actions: &[Action]) -> Vec<u8> {
    let frames: Vec<Vec<u8>> = sent(actions)
        .into_iter()
        .filter(|(kind, _)| *kind == FrameKind::Eapol)
        .map(|(_, f)| f)
        .collect();
    assert_eq!(frames.len(), 1, "{actions:?}");
    frames.into_iter().next().unwrap()
}

pub(crate) fn leaves(actions: &[Action]) -> Vec<u16> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Leave { reason } => Some(*reason),
            _ => None,
        })
        .collect()
}

/// Drive an open join of `ap` to `Connected`, from the start.
pub(crate) fn connect_open(h: &mut H, ap: &Ap) {
    let actions = h.begin();
    let actions = h.answer_scan(actions, &[ap]);
    assert!(matches!(actions[0], Action::Join { .. }), "{actions:?}");
    let actions = h.rx(FrameKind::Mgmt, ap.auth_response(0));
    assert_eq!(h.state().0, ConnState::Associating, "{actions:?}");
    let actions = h.rx(FrameKind::Mgmt, ap.assoc_response(0, 1));
    assert!(matches!(
        actions.last(),
        Some(Action::SetState {
            state: StaState::Authorized,
            ..
        })
    ));
    assert_eq!(h.state(), (ConnState::Connected, FailReason::None));
}

/// Drive a WPA2 join of `ap` with `pmk` up to the handshake.
pub(crate) fn to_handshake(h: &mut H, ap: &Ap, pmk: [u8; 32]) {
    let actions = h.begin();
    let actions = h.answer_scan(actions, &[ap]);
    let actions = h.answer_pmk(actions, pmk);
    assert!(matches!(actions[0], Action::Join { .. }));
    h.rx(FrameKind::Mgmt, ap.auth_response(0));
    let actions = h.rx(FrameKind::Mgmt, ap.assoc_response(0, 1));
    assert!(matches!(
        actions.as_slice(),
        [Action::SetState {
            state: StaState::Associated,
            aid: 1
        }]
    ));
    assert_eq!(h.state().0, ConnState::Handshake);
}

/// Run the 4-way handshake to `Connected`.
pub(crate) fn connect_wpa2(h: &mut H, ap: &mut Ap) {
    let pmk = ap.pmk;
    to_handshake(h, ap, pmk);
    let actions = h.rx(FrameKind::Eapol, ap.msg1());
    assert!(ap.take_msg2(&eapol_sent(&actions)));
    let actions = h.rx(FrameKind::Eapol, ap.msg3(&[0x42; 16]));
    assert_eq!(h.state(), (ConnState::Connected, FailReason::None), "{actions:?}");
}
