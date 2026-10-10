//! A test authenticator built from `lazyos_crypto::wifi` and `raw_frame`
//! alone (no `KeyFrame`, no `Supplicant` code path), and the handshakes
//! against it.

use std::vec::Vec;

use ieee80211::Akm;
use lazyos_crypto::wifi;

use super::*;
use crate::{Action, Error, Stage, MAX_PENDING};

pub(crate) struct Authenticator {
    pub akm: Akm,
    pub pmk: [u8; 32],
    pub anonce: [u8; 32],
    pub replay: u64,
    pub ptk: [u8; 48],
    pub rsn_ie: Vec<u8>,
}

fn sha256(akm: Akm) -> bool {
    akm == Akm::PskSha256
}

impl Authenticator {
    pub fn new(akm: Akm, pmk: [u8; 32], anonce: [u8; 32]) -> Authenticator {
        Authenticator {
            akm,
            pmk,
            anonce,
            replay: 0,
            ptk: [0; 48],
            rsn_ie: rsn_ie(akm),
        }
    }

    pub fn version(&self) -> u8 {
        if sha256(self.akm) {
            3
        } else {
            2
        }
    }

    fn mic(&self, frame: &[u8]) -> [u8; 16] {
        let kck: [u8; 16] = self.ptk[..16].try_into().unwrap();
        if sha256(self.akm) {
            wifi::aes_cmac_128(&kck, frame)
        } else {
            wifi::hmac_sha1_128(&kck, frame)
        }
    }

    pub fn seal(&self, mut frame: Vec<u8>) -> Vec<u8> {
        frame[81..97].fill(0);
        let mic = self.mic(&frame);
        frame[81..97].copy_from_slice(&mic);
        frame
    }

    fn derive(&mut self, snonce: &[u8; 32]) {
        let (lo_a, hi_a) = (AA.min(SPA), AA.max(SPA));
        let (lo_n, hi_n) = (self.anonce.min(*snonce), self.anonce.max(*snonce));
        let data = [&lo_a[..], &hi_a, &lo_n, &hi_n].concat();
        let kdf = if sha256(self.akm) {
            wifi::kdf_sha256
        } else {
            wifi::prf_sha1
        };
        kdf(&self.pmk, b"Pairwise key expansion", &data, &mut self.ptk).unwrap();
    }

    /// Derive the PTK from the SNonce in a message 2, with no checks.
    pub fn derive_for_test(&mut self, msg2: &[u8]) {
        let snonce: [u8; 32] = msg2[17..49].try_into().unwrap();
        self.derive(&snonce);
    }

    pub fn msg1(&mut self) -> Vec<u8> {
        self.replay += 1;
        raw_frame(
            self.version(),
            0x0088,
            16,
            self.replay,
            &self.anonce,
            [0; 8],
            &[],
            [0; 16],
        )
    }

    /// Check message 2 as the standard describes it; keep its SNonce.
    pub fn take_msg2(&mut self, pdu: &[u8]) {
        let snonce: [u8; 32] = pdu[17..49].try_into().unwrap();
        self.derive(&snonce);
        assert_eq!(
            u16::from_be_bytes([pdu[5], pdu[6]]),
            u16::from(self.version()) | 0x0108,
            "message 2 key info"
        );
        assert_eq!(
            u64::from_be_bytes(pdu[9..17].try_into().unwrap()),
            self.replay
        );
        assert_eq!(
            &pdu[99..],
            &self.rsn_ie[..],
            "message 2 carries the association RSN element"
        );
        let mut zeroed = pdu.to_vec();
        zeroed[81..97].fill(0);
        assert_eq!(self.mic(&zeroed), pdu[81..97], "message 2 MIC");
    }

    pub fn msg3(&mut self, rsn_ie: &[u8], index: u8, gtk: &[u8]) -> Vec<u8> {
        self.replay += 1;
        let kek: [u8; 16] = self.ptk[16..32].try_into().unwrap();
        let wrapped = wifi::aes_wrap(&kek, &msg3_plain(rsn_ie, index, gtk)).unwrap();
        self.seal(raw_frame(
            self.version(),
            0x13C8,
            16,
            self.replay,
            &self.anonce,
            [7; 8],
            &wrapped,
            [0; 16],
        ))
    }

    pub fn take_msg4(&self, pdu: &[u8]) {
        assert_eq!(
            u16::from_be_bytes([pdu[5], pdu[6]]),
            u16::from(self.version()) | 0x0308
        );
        assert_eq!(
            u64::from_be_bytes(pdu[9..17].try_into().unwrap()),
            self.replay
        );
        assert!(pdu[17..49].iter().all(|&b| b == 0) && pdu.len() == 99);
        let mut zeroed = pdu.to_vec();
        zeroed[81..97].fill(0);
        assert_eq!(self.mic(&zeroed), pdu[81..97], "message 4 MIC");
    }

    pub fn group1(&mut self, index: u8, gtk: &[u8]) -> Vec<u8> {
        self.replay += 1;
        let kek: [u8; 16] = self.ptk[16..32].try_into().unwrap();
        let kde = [
            &[0xDD, 6 + gtk.len() as u8, 0x00, 0x0F, 0xAC, 1, index & 3, 0][..],
            gtk,
        ]
        .concat();
        let wrapped = wifi::aes_wrap(&kek, &pad(kde)).unwrap();
        self.seal(raw_frame(
            self.version(),
            0x1380,
            16,
            self.replay,
            &[0x99; 32],
            [8; 8],
            &wrapped,
            [0; 16],
        ))
    }

    pub fn take_group2(&self, pdu: &[u8]) {
        assert_eq!(
            u16::from_be_bytes([pdu[5], pdu[6]]),
            u16::from(self.version()) | 0x0300
        );
        assert_eq!(
            u64::from_be_bytes(pdu[9..17].try_into().unwrap()),
            self.replay
        );
        let mut zeroed = pdu.to_vec();
        zeroed[81..97].fill(0);
        assert_eq!(self.mic(&zeroed), pdu[81..97], "group message 2 MIC");
    }

    pub fn tk(&self) -> [u8; 16] {
        self.ptk[32..].try_into().unwrap()
    }
}

pub(crate) const PMK: [u8; 32] = [0x5A; 32];
pub(crate) const SNONCE: [u8; 32] = [0x77; 32];
pub(crate) const ANONCE: [u8; 32] = [0x33; 32];
pub(crate) const GTK: [u8; 16] = [0x42; 16];

pub(crate) fn only_send(actions: Vec<Action>) -> Vec<u8> {
    let mut actions = actions;
    assert_eq!(actions.len(), 1, "{actions:?}");
    match actions.pop() {
        Some(Action::Send(pdu)) => pdu,
        other => panic!("expected a send, got {other:?}"),
    }
}

/// Run messages 1 to 4; returns the actions of message 3.
pub(crate) fn pairwise(
    sup: &mut Supplicant<impl crate::Crypto>,
    auth: &mut Authenticator,
) -> Vec<Action> {
    let msg2 = only_send(sup.input(&auth.msg1()).unwrap());
    auth.take_msg2(&msg2);
    assert_eq!(sup.stage(), Stage::WaitMsg3);
    let ie = auth.rsn_ie.clone();
    sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap()
}

#[test]
fn four_way_and_group_handshake_for_both_akms() {
    for akm in [Akm::Psk, Akm::PskSha256] {
        let mut sup = real(akm, PMK, SNONCE);
        let mut auth = Authenticator::new(akm, PMK, ANONCE);
        assert_eq!(sup.stage(), Stage::Start);
        let mut actions = pairwise(&mut sup, &mut auth);
        assert_eq!(sup.stage(), Stage::Established);
        assert_eq!(actions.len(), 4);
        let Action::Send(msg4) = actions.remove(0) else {
            panic!()
        };
        auth.take_msg4(&msg4);
        let Action::InstallPtk { cipher, tk } = actions.remove(0) else {
            panic!()
        };
        assert_eq!(
            (cipher, *tk.expose()),
            (ieee80211::Cipher::Ccmp128, auth.tk())
        );
        let Action::InstallGtk {
            index,
            tx,
            key,
            rsc,
        } = actions.remove(0)
        else {
            panic!()
        };
        assert_eq!((index, tx, *key.expose(), rsc), (1, false, GTK, [7; 8]));
        assert_eq!(actions.remove(0), Action::Authorized);

        // Group rekey: a new GTK arrives, message 2 goes back.
        let new_gtk = [0x61; 16];
        let mut actions = sup.input(&auth.group1(2, &new_gtk)).unwrap();
        assert_eq!(actions.len(), 2);
        let Action::Send(group2) = actions.remove(0) else {
            panic!()
        };
        auth.take_group2(&group2);
        let Action::InstallGtk {
            index, key, rsc, ..
        } = actions.remove(0)
        else {
            panic!()
        };
        assert_eq!((index, *key.expose(), rsc), (2, new_gtk, [8; 8]));
    }
}

#[test]
fn a_repeated_group_key_is_answered_but_not_reinstalled() {
    let mut sup = real(Akm::Psk, PMK, SNONCE);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    pairwise(&mut sup, &mut auth);
    // Same index and key as message 3 delivered, twice (higher counter).
    for _ in 0..2 {
        let group1 = auth.group1(1, &GTK);
        let actions = sup.input(&group1).unwrap();
        auth.take_group2(&only_send(actions));
    }
}

#[test]
fn a_retransmitted_message_3_gets_message_4_and_installs_nothing() {
    let mut sup = real(Akm::Psk, PMK, SNONCE);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    pairwise(&mut sup, &mut auth);
    // The authenticator never saw message 4: it sends message 3 again with the
    // next replay counter.
    let ie = auth.rsn_ie.clone();
    let again = only_send(sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap());
    auth.take_msg4(&again);
}

#[test]
fn retransmitted_message_1_reuses_the_snonce() {
    let mut sup = real(Akm::Psk, PMK, SNONCE);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    let first = only_send(sup.input(&auth.msg1()).unwrap());
    let second = only_send(sup.input(&auth.msg1()).unwrap());
    assert_eq!(first[17..49], second[17..49], "same SNonce");
    auth.take_msg2(&second);
    // Message 3 (replay counter above both message 1s) completes it.
    let ie = auth.rsn_ie.clone();
    assert_eq!(sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap().len(), 4);
}

#[test]
fn a_forged_message_1_cannot_break_a_genuine_handshake() {
    let mut sup = real(Akm::Psk, PMK, SNONCE);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    let msg1 = auth.msg1();
    let msg2 = only_send(sup.input(&msg1).unwrap());
    auth.take_msg2(&msg2);
    // The attacker injects message 1s with other ANonces and a huge counter.
    let mut forger = Authenticator::new(Akm::Psk, [0xEE; 32], [0xF0; 32]);
    forger.replay = 1000;
    sup.input(&forger.msg1()).unwrap();
    forger.anonce = [0xF1; 32];
    sup.input(&forger.msg1()).unwrap();
    assert_eq!(sup.state().pending_count(), 3);
    // The genuine message 3 still succeeds, and the candidates are dropped.
    let ie = auth.rsn_ie.clone();
    let actions = sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap();
    assert_eq!(actions.len(), 4);
    assert_eq!(sup.state().pending_count(), 0);
}

#[test]
fn a_message_3_for_a_forged_candidate_changes_nothing() {
    let mut sup = real(Akm::Psk, PMK, SNONCE);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    let msg1 = auth.msg1();
    let msg2 = only_send(sup.input(&msg1).unwrap());
    auth.take_msg2(&msg2);
    // A forged message 1 whose message 3 the attacker cannot MAC (it does not
    // know the PMK): refused by MIC, and the genuine candidate survives.
    let mut forger = Authenticator::new(Akm::Psk, [0xEE; 32], [0xF0; 32]);
    let forged = forger.msg1();
    let fm2 = only_send(sup.input(&forged).unwrap());
    forger.derive_for_test(&fm2); // its own PMK, so not what the supplicant used
    let ie = forger.rsn_ie.clone();
    let before = sup.state().clone();
    assert_eq!(
        sup.input(&forger.msg3(&ie, 1, &GTK)).unwrap_err(),
        Error::BadMic
    );
    assert!(sup.state() == &before);
    let ie = auth.rsn_ie.clone();
    assert_eq!(sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap().len(), 4);
}

#[test]
fn a_flood_of_message_1s_stays_bounded_and_the_newest_exchange_wins() {
    let mut sup = real(Akm::Psk, PMK, SNONCE);
    let mut genuine = Authenticator::new(Akm::Psk, PMK, ANONCE);
    let msg1 = genuine.msg1();
    let msg2 = only_send(sup.input(&msg1).unwrap());
    genuine.take_msg2(&msg2);
    let mut forger = Authenticator::new(Akm::Psk, [0xEE; 32], [0; 32]);
    forger.replay = 50;
    for n in 1..=(MAX_PENDING as u8 + 10) {
        forger.anonce = [n; 32];
        sup.input(&forger.msg1()).unwrap();
        assert!(sup.state().pending_count() <= MAX_PENDING);
    }
    // The flood evicted the first exchange (the limit: a flood costs the
    // caller a retry), which is now unknown to the supplicant.
    let ie = genuine.rsn_ie.clone();
    let m3 = genuine.msg3(&ie, 1, &GTK);
    assert_eq!(sup.input(&m3).unwrap_err(), Error::AnonceChanged);
    // The authenticator retries message 1 (same ANonce, higher counter): the
    // newest genuine exchange is accepted even right after the flood.
    let msg1 = genuine.msg1();
    let msg2 = only_send(sup.input(&msg1).unwrap());
    genuine.take_msg2(&msg2);
    assert_eq!(sup.state().pending_count(), MAX_PENDING);
    let m3 = genuine.msg3(&ie, 1, &GTK);
    assert_eq!(sup.input(&m3).unwrap().len(), 4);
}

#[test]
fn ptk_rekey_keeps_the_old_key_until_message_3_verifies() {
    let mut calls = 0u8;
    let nonce = move || {
        calls += 1;
        [calls; 32]
    };
    let mut sup = Supplicant::new(config(Akm::Psk, PMK), Standard::new(nonce)).unwrap();
    let mut first = Authenticator::new(Akm::Psk, PMK, [0x01; 32]);
    pairwise(&mut sup, &mut first);
    // The AP starts a rekey: new ANonce, counters continue upward.
    let mut second = Authenticator::new(Akm::Psk, PMK, [0x02; 32]);
    second.replay = 10;
    let msg2 = only_send(sup.input(&second.msg1()).unwrap());
    second.take_msg2(&msg2);
    assert!(sup.state().has_pending());
    // A group message under the installed PTK is still understood meanwhile.
    let group1 = first.group1(1, &GTK);
    first.take_group2(&only_send(sup.input(&group1).unwrap()));
    let ie = second.rsn_ie.clone();
    let actions = sup.input(&second.msg3(&ie, 1, &GTK)).unwrap();
    assert!(matches!(actions[1], Action::InstallPtk { .. }));
    // The old PTK no longer verifies anything.
    assert_eq!(
        sup.input(&first.group1(1, &GTK)).unwrap_err(),
        Error::BadMic
    );
}
