//! The supplicant against the independent Python transcript
//! (`tools/wifi/make_eapol_vectors.py`): byte-for-byte replies, both AKMs.

use ieee80211::Akm;
use lazyos_crypto::wifi;

use super::vectors::*;
use super::{real, rsn_ie, unhex, unhex_array};
use crate::{Action, KeyFrame};
use crate::{Key, Standard};

#[test]
fn pmk_from_the_passphrase_matches_python() {
    // The mechanism of IEEE 802.11-2020 Annex J.4 (whose own vector is
    // checked in libs/crypto), on this transcript's inputs.
    let pmk = wifi::pbkdf2_sha1(PASSPHRASE.as_bytes(), SSID.as_bytes()).unwrap();
    for t in &TRANSCRIPTS {
        assert_eq!(pmk[..], unhex(t.pmk)[..]);
    }
}

#[test]
fn ptk_derivation_matches_python_for_both_akms() {
    for t in &TRANSCRIPTS {
        let akm = if t.akm == 6 { Akm::PskSha256 } else { Akm::Psk };
        let crypto = Standard::new(|| [0; 32]);
        let ptk = crate::Crypto::derive_ptk(
            &crypto,
            akm,
            &Key::new(unhex_array(t.pmk)),
            &unhex_array(AA),
            &unhex_array(SPA),
            &unhex_array(ANONCE),
            &unhex_array(SNONCE),
        );
        assert_eq!(ptk.expose()[..], unhex(t.ptk)[..], "akm {}", t.akm);
        assert_eq!(ptk.expose()[32..], unhex(t.tk)[..]);
    }
}

#[test]
fn replies_equal_the_python_authenticator_transcript() {
    for t in &TRANSCRIPTS {
        let akm = if t.akm == 6 { Akm::PskSha256 } else { Akm::Psk };
        assert_eq!(rsn_ie(akm), unhex(t.ap_rsn_ie));
        let mut sup = real(akm, unhex_array(t.pmk), unhex_array(SNONCE));

        let actions = sup.input(&unhex(t.msg1)).unwrap();
        assert_eq!(
            actions,
            [Action::Send(unhex(t.msg2))],
            "akm {} message 2",
            t.akm
        );

        let mut actions = sup.input(&unhex(t.msg3)).unwrap();
        assert_eq!(actions.len(), 4);
        assert_eq!(
            actions.remove(0),
            Action::Send(unhex(t.msg4)),
            "akm {} message 4",
            t.akm
        );
        let Action::InstallPtk { tk, .. } = actions.remove(0) else {
            panic!()
        };
        assert_eq!(tk.expose()[..], unhex(t.tk)[..]);
        let Action::InstallGtk {
            index,
            tx,
            key,
            rsc,
        } = actions.remove(0)
        else {
            panic!()
        };
        assert_eq!((index, tx), (GTK_INDEX, false));
        assert_eq!(key.expose()[..], unhex(GTK)[..]);
        assert_eq!(rsc[..], unhex(MSG3_RSC)[..]);

        let mut actions = sup.input(&unhex(t.group1)).unwrap();
        assert_eq!(
            actions.remove(0),
            Action::Send(unhex(t.group2)),
            "akm {} group 2",
            t.akm
        );
        let Action::InstallGtk {
            index, key, rsc, ..
        } = actions.remove(0)
        else {
            panic!()
        };
        assert_eq!(index, NEW_GTK_INDEX);
        assert_eq!(key.expose()[..], unhex(NEW_GTK)[..]);
        assert_eq!(rsc[..], unhex(GROUP_RSC)[..]);
    }
}

#[test]
fn transcript_frames_parse_and_encode_to_themselves() {
    for t in &TRANSCRIPTS {
        let version = if t.akm == 6 { 3 } else { 2 };
        for (name, hex, counter) in [
            ("msg1", t.msg1, COUNTERS[0]),
            ("msg2", t.msg2, COUNTERS[0]),
            ("msg3", t.msg3, COUNTERS[1]),
            ("msg4", t.msg4, COUNTERS[1]),
            ("group1", t.group1, COUNTERS[2]),
            ("group2", t.group2, COUNTERS[2]),
        ] {
            let pdu = unhex(hex);
            let frame = KeyFrame::parse(&pdu).unwrap();
            assert_eq!(frame.encode().unwrap(), pdu, "{name}");
            assert_eq!(
                (frame.version(), frame.replay_counter()),
                (version, counter),
                "{name}"
            );
        }
    }
}

/// The Annex J inputs the handshake rests on, run through the same functions
/// the supplicant calls: J.4 (passphrase "password", SSID "IEEE") for the PMK
/// and J.3 (PRF test case 1) for the key expansion. The full vectors live in
/// `libs/crypto`; this keeps the clause cited beside the handshake.
#[test]
fn annex_j_vectors_under_the_handshake_primitives() {
    let pmk = wifi::pbkdf2_sha1(b"password", b"IEEE").unwrap();
    assert_eq!(
        pmk[..],
        unhex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")[..]
    );
    let mut out = [0u8; 24];
    wifi::prf_sha1(&[0x0b; 20], b"prefix", b"Hi There", &mut out).unwrap();
    assert_eq!(
        out[..],
        unhex("bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606")[..]
    );
}
