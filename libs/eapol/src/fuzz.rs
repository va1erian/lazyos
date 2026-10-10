//! Fuzz entry point, shared by the seeded tests below and the cargo-fuzz
//! target (`fuzz/fuzz_targets/eapol.rs`).
//!
//! [`run`] takes any bytes and uses them three ways:
//!
//! * as one EAPOL-Key PDU and as key data: parsing must not panic, and a PDU
//!   that parses must encode back to the same octets;
//! * as a script for the supplicant: the first octet picks the AKM (bit 0),
//!   then frames, each a big-endian `u16` length and that many octets. The
//!   supplicant runs on [`ScriptCrypto`], whose MIC is the constant `0xAA * 16`
//!   and whose "unwrap" drops the first 8 octets, so a mutated seed reaches
//!   past the MIC check into the key data. After every frame: an `Err` left
//!   the state as it was, the replay counter never went down, and the actions
//!   fit the stage.

use std::vec::Vec;

use ieee80211::{Akm, Cipher, Rsn};

use crate::crypto::{ptk_data, Crypto, PTK_LEN};
use crate::key::KeyFrame;
use crate::secret::Key;
use crate::supplicant::{Config, Supplicant};
use crate::{keydata, Action};

/// The MIC every [`ScriptCrypto`] frame must carry to pass.
pub const SCRIPT_MIC: [u8; 16] = [0xAA; 16];

/// Crypto for the fuzz target: deterministic and trivially forgeable.
pub struct ScriptCrypto {
    counter: u8,
}

impl Crypto for ScriptCrypto {
    fn random_nonce(&mut self) -> [u8; 32] {
        self.counter = self.counter.wrapping_add(1);
        [self.counter; 32]
    }

    fn derive_ptk(
        &self,
        _akm: Akm,
        pmk: &Key<32>,
        aa: &[u8; 6],
        spa: &[u8; 6],
        anonce: &[u8; 32],
        snonce: &[u8; 32],
    ) -> Key<PTK_LEN> {
        let data = ptk_data(aa, spa, anonce, snonce);
        let mut out = [0u8; PTK_LEN];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = data[i % data.len()] ^ pmk.expose()[i % 32];
        }
        Key::new(out)
    }

    fn mic(&self, _akm: Akm, _kck: &Key<16>, _pdu: &[u8]) -> [u8; 16] {
        SCRIPT_MIC
    }

    fn key_unwrap(&self, _kek: &Key<16>, wrapped: &[u8]) -> Option<Vec<u8>> {
        (wrapped.len() >= 24 && wrapped.len().is_multiple_of(8)).then(|| wrapped[8..].to_vec())
    }
}

/// A supplicant on [`ScriptCrypto`] for `akm` (2 or 6).
pub fn script_supplicant(psk_sha256: bool) -> Supplicant<ScriptCrypto> {
    let (akm, rsn) = if psk_sha256 {
        (Akm::PskSha256, Rsn::wpa2_psk_sha256())
    } else {
        (Akm::Psk, Rsn::wpa2_psk())
    };
    let ie = rsn.to_ie().expect("a one-suite RSN element fits");
    let config = Config {
        akm,
        pairwise: Cipher::Ccmp128,
        group: Cipher::Ccmp128,
        pmk: Key::new([7; 32]),
        aa: [2, 1, 1, 1, 1, 1],
        spa: [2, 2, 2, 2, 2, 2],
        ap_rsn_ie: ie.clone(),
        assoc_rsn_ie: ie,
    };
    Supplicant::new(config, ScriptCrypto { counter: 0 }).expect("a valid configuration")
}

/// Run every check on `data`.
pub fn run(data: &[u8]) {
    frame_checks(data);
    let _ = keydata::parse(data);
    script(data);
}

fn frame_checks(data: &[u8]) {
    let Ok(frame) = KeyFrame::parse(data) else {
        return;
    };
    let again = frame.encode().expect("a parsed frame encodes");
    assert_eq!(
        &again[..],
        &data[..again.len()],
        "encode is the inverse of parse"
    );
    assert_eq!(KeyFrame::parse(&again).as_ref(), Ok(&frame));
    assert_eq!(frame.mic_input().expect("encodes").len(), again.len());
}

fn script(data: &[u8]) {
    let Some((&first, mut rest)) = data.split_first() else {
        return;
    };
    let mut sup = script_supplicant(first & 1 != 0);
    let mut last_replay = None;
    while let Some((len, tail)) = rest.split_first_chunk::<2>() {
        let len = usize::from(u16::from_be_bytes(*len)).min(tail.len());
        let (pdu, tail) = tail.split_at(len);
        rest = tail;
        let before = sup.state().clone();
        match sup.input(pdu) {
            Err(_) => assert!(sup.state() == &before, "an Err changed the state"),
            Ok(actions) => check_actions(&actions, before.has_pending()),
        }
        let replay = sup.state().rx_replay();
        assert!(replay >= last_replay, "the replay counter went down");
        last_replay = replay;
    }
}

fn check_actions(actions: &[Action], pending_before: bool) {
    let count = |f: fn(&Action) -> bool| actions.iter().filter(|a| f(a)).count();
    assert!(
        matches!(actions.first(), Some(Action::Send(_))),
        "a reply comes first"
    );
    let ptks = count(|a| matches!(a, Action::InstallPtk { .. }));
    assert!(ptks <= 1 && count(|a| matches!(a, Action::Authorized)) == ptks);
    assert!(
        ptks == 0 || pending_before,
        "a PTK installed without a pending one"
    );
    assert!(count(|a| matches!(a, Action::InstallGtk { .. })) <= 1);
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::for_seeds;

    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    #[test]
    fn checked_in_seeds_replay() {
        replay("eapol", run);
    }

    #[test]
    fn random_bytes() {
        for_seeds("eapol_random_bytes", |_, rng| {
            let len = rng.below(300) as usize;
            run(&rng.bytes(len));
        });
    }

    /// Valid handshakes (under [`ScriptCrypto`]) with bits flipped and tails cut.
    #[test]
    fn mutated_handshakes() {
        for_seeds("eapol_mutated_handshakes", |_, rng| {
            let mut script = crate::tests::scripted_handshake(rng.one_in(2));
            let flips = rng.below(8) as usize;
            rng.flip_bits(&mut script, flips);
            let cut = rng.below(script.len() as u64 / 4 + 1) as usize;
            script.truncate(script.len() - cut);
            run(&script);
        });
    }
}
