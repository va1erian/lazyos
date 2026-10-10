//! Every refusal leaves the supplicant exactly as it was, and a good frame
//! still works afterwards.

use std::boxed::Box;
use std::vec::Vec;

use ieee80211::{Akm, Cipher, Rsn, RsnCaps};
use lazyos_crypto::wifi;

use super::auth::*;
use super::*;
use crate::{Crypto, Error, Stage};

type Sup = Supplicant<Standard<Box<dyn FnMut() -> [u8; 32]>>>;

fn supplicant(akm: Akm) -> Sup {
    Supplicant::new(
        config(akm, PMK),
        Standard::new(Box::new(|| SNONCE) as Box<dyn FnMut() -> [u8; 32]>),
    )
    .unwrap()
}

/// Refused with `error`, and nothing changed.
fn refuse(sup: &mut Sup, pdu: &[u8], error: Error) {
    let before = sup.state().clone();
    assert_eq!(sup.input(pdu).unwrap_err(), error);
    assert!(sup.state() == &before, "{error:?} changed the state");
}

/// A supplicant that has answered message 1.
fn waiting(akm: Akm) -> (Sup, Authenticator) {
    let mut sup = supplicant(akm);
    let mut auth = Authenticator::new(akm, PMK, ANONCE);
    let msg1 = auth.msg1();
    let msg2 = only_send(sup.input(&msg1).unwrap());
    auth.take_msg2(&msg2);
    assert_eq!(sup.stage(), Stage::WaitMsg3);
    (sup, auth)
}

fn established(akm: Akm) -> (Sup, Authenticator) {
    let (mut sup, mut auth) = waiting(akm);
    let ie = auth.rsn_ie.clone();
    sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap();
    assert_eq!(sup.stage(), Stage::Established);
    (sup, auth)
}

/// Message 3 with an arbitrary key data plaintext, wrapped under the real KEK.
fn msg3_with(auth: &mut Authenticator, plain: &[u8]) -> Vec<u8> {
    auth.replay += 1;
    let kek: [u8; 16] = auth.ptk[16..32].try_into().unwrap();
    let wrapped = wifi::aes_wrap(&kek, plain).unwrap();
    auth.seal(raw_frame(
        auth.version(),
        0x13C8,
        16,
        auth.replay,
        &auth.anonce,
        [7; 8],
        &wrapped,
        [0; 16],
    ))
}

#[test]
fn wrong_mic_is_refused_then_the_good_frame_works() {
    for akm in [Akm::Psk, Akm::PskSha256] {
        let (mut sup, mut auth) = waiting(akm);
        let ie = auth.rsn_ie.clone();
        let good = auth.msg3(&ie, 1, &GTK);
        for bit in [81 * 8, 88 * 8 + 3, 96 * 8 + 7] {
            let mut bad = good.clone();
            bad[bit / 8] ^= 1 << (bit % 8);
            refuse(&mut sup, &bad, Error::BadMic);
        }
        // Anything else flipped also breaks the MIC: nonce, replay, key data.
        for at in [10, 20, 70, 100, 120] {
            let mut bad = good.clone();
            bad[at] ^= 0x10;
            // The ANonce selects the candidate, so a changed one matches none.
            let error = if (17..49).contains(&at) {
                Error::AnonceChanged
            } else {
                Error::BadMic
            };
            refuse(&mut sup, &bad, error);
        }
        assert_eq!(sup.input(&good).unwrap().len(), 4);
    }
}

#[test]
fn wrong_passphrase_is_a_mic_failure_not_a_panic() {
    let mut sup = supplicant(Akm::Psk);
    let mut auth = Authenticator::new(Akm::Psk, [0x01; 32], ANONCE);
    let msg2 = only_send(sup.input(&auth.msg1()).unwrap());
    assert_ne!(msg2[81..97], [0; 16]);
    auth.derive_for_test(&msg2);
    let ie = auth.rsn_ie.clone();
    refuse(&mut sup, &auth.msg3(&ie, 1, &GTK), Error::BadMic);
    assert!(!Error::BadMic.is_fatal());
}

#[test]
fn replayed_and_non_increasing_counters() {
    let mut sup = supplicant(Akm::Psk);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    auth.replay = 4;
    let msg1 = auth.msg1(); // counter 5
    let msg2 = only_send(sup.input(&msg1).unwrap());
    auth.take_msg2(&msg2);
    let ie = auth.rsn_ie.clone();
    // Message 3 must beat message 1's counter: equal and lower are refused.
    auth.replay = 4;
    refuse(&mut sup, &auth.msg3(&ie, 1, &GTK), Error::Replay);
    auth.replay = 2;
    refuse(&mut sup, &auth.msg3(&ie, 1, &GTK), Error::Replay);
    auth.replay = 5;
    assert_eq!(sup.input(&auth.msg3(&ie, 1, &GTK)).unwrap().len(), 4); // counter 6

    // After the exchange a group message replays or lowers the counter.
    let group = auth.group1(1, &[9; 16]); // counter 7
    sup.input(&group).unwrap();
    refuse(&mut sup, &group, Error::Replay);
    auth.replay = 3;
    refuse(&mut sup, &auth.group1(1, &[8; 16]), Error::Replay);
    // A message 1 that does not beat the last verified counter.
    let mut stale = Authenticator::new(Akm::Psk, PMK, [0x55; 32]);
    refuse(&mut sup, &stale.msg1(), Error::Replay);
}

#[test]
fn rsn_element_mismatch_is_a_fatal_downgrade() {
    let akm = Akm::Psk;
    let downgrades = [
        Rsn {
            pairwise: std::vec![Cipher::Tkip, Cipher::Ccmp128],
            ..Rsn::wpa2_psk()
        },
        Rsn {
            akms: std::vec![Akm::Psk, Akm::PskSha256],
            ..Rsn::wpa2_psk()
        },
        Rsn {
            caps: RsnCaps(RsnCaps::MFPC),
            ..Rsn::wpa2_psk()
        },
        Rsn {
            group: Cipher::Tkip,
            ..Rsn::wpa2_psk()
        },
    ];
    for rsn in downgrades {
        let (mut sup, mut auth) = waiting(akm);
        let forged = rsn.to_ie().unwrap();
        refuse(&mut sup, &auth.msg3(&forged, 1, &GTK), Error::RsnMismatch);
    }
    // One flipped bit in the right element.
    let (mut sup, mut auth) = waiting(akm);
    let mut one_bit = rsn_ie(akm);
    one_bit[10] ^= 0x01;
    refuse(&mut sup, &auth.msg3(&one_bit, 1, &GTK), Error::RsnMismatch);
    assert!(Error::RsnMismatch.is_fatal());
    assert_eq!(Error::RsnMismatch.deauth_reason(), Some(17));
    // No RSN element at all.
    let (mut sup, mut auth) = waiting(akm);
    let plain = pad([&[0xDD, 22, 0, 0x0F, 0xAC, 1, 1, 0][..], &GTK].concat());
    refuse(&mut sup, &msg3_with(&mut auth, &plain), Error::MissingRsn);
}

#[test]
fn truncated_or_bad_key_data_in_message_3() {
    let akm = Akm::Psk;
    let ie = rsn_ie(akm);
    let gtk_kde = [&[0xDD, 22, 0, 0x0F, 0xAC, 1, 1, 0][..], &GTK].concat();
    // Cut the GTK KDE short (still a multiple of 8 to be wrappable).
    let cut = [&ie[..], &gtk_kde[..gtk_kde.len() - 6]].concat();
    assert_eq!(cut.len() % 8, 0);
    let (mut sup, mut auth) = waiting(akm);
    refuse(&mut sup, &msg3_with(&mut auth, &cut), Error::BadKeyData);
    // A GTK that is not 16 octets.
    let (mut sup, mut auth) = waiting(akm);
    let short = pad([&ie[..], &[0xDD, 14, 0, 0x0F, 0xAC, 1, 1, 0], &GTK[..8]].concat());
    refuse(&mut sup, &msg3_with(&mut auth, &short), Error::BadGtk);
    // No GTK KDE.
    let (mut sup, mut auth) = waiting(akm);
    refuse(
        &mut sup,
        &msg3_with(&mut auth, &pad(ie.clone())),
        Error::MissingGtk,
    );
    // Non-zero padding.
    let (mut sup, mut auth) = waiting(akm);
    let mut dirty = msg3_plain(&ie, 1, &GTK);
    let last = dirty.len() - 1;
    dirty[last] = 1;
    refuse(&mut sup, &msg3_with(&mut auth, &dirty), Error::BadKeyData);
    // Wrapped under the wrong KEK: the MIC is right, the unwrap is not.
    let (mut sup, mut auth) = waiting(akm);
    auth.replay += 1;
    let wrapped = wifi::aes_wrap(&[0xEE; 16], &msg3_plain(&ie, 1, &GTK)).unwrap();
    let pdu = auth.seal(raw_frame(
        2,
        0x13C8,
        16,
        auth.replay,
        &auth.anonce,
        [7; 8],
        &wrapped,
        [0; 16],
    ));
    refuse(&mut sup, &pdu, Error::BadKeyData);
    // Key data that is not a multiple of 8, or empty.
    for data in [&[1u8; 23][..], &[]] {
        let (mut sup, mut auth) = waiting(akm);
        auth.replay += 1;
        let pdu = auth.seal(raw_frame(
            2,
            0x13C8,
            16,
            auth.replay,
            &auth.anonce,
            [7; 8],
            data,
            [0; 16],
        ));
        refuse(&mut sup, &pdu, Error::BadKeyData);
    }
    // The wrong Key Length.
    let (mut sup, mut auth) = waiting(akm);
    auth.replay += 1;
    let kek: [u8; 16] = auth.ptk[16..32].try_into().unwrap();
    let wrapped = wifi::aes_wrap(&kek, &msg3_plain(&ie, 1, &GTK)).unwrap();
    let pdu = auth.seal(raw_frame(
        2,
        0x13C8,
        32,
        auth.replay,
        &auth.anonce,
        [7; 8],
        &wrapped,
        [0; 16],
    ));
    refuse(&mut sup, &pdu, Error::BadKeyLength(32));
}

#[test]
fn wrong_key_info_combinations() {
    let (mut sup, mut auth) = waiting(Akm::Psk);
    let ie = auth.rsn_ie.clone();
    let good = auth.msg3(&ie, 1, &GTK);
    let flip = |pdu: &[u8], flag: u16| {
        let mut p = pdu.to_vec();
        let info = u16::from_be_bytes([p[5], p[6]]) ^ flag;
        p[5..7].copy_from_slice(&info.to_be_bytes());
        p
    };
    // Each missing or extra flag on message 3.
    for flag in [
        0x0040, 0x0080, 0x0100, 0x0200, 0x0400, 0x0800, 0x1000, 0x2000, 0x0008,
    ] {
        let bad = flip(&good, flag);
        refuse(
            &mut sup,
            &bad,
            Error::BadKeyInfo(u16::from_be_bytes([bad[5], bad[6]])),
        );
    }
    // Message 2 and 4 shaped frames are not for a supplicant.
    for flags in [0x0108, 0x0308, 0x0300, 0x0000, 0x0488, 0x0888] {
        let pdu = raw_frame(2, flags, 16, 9, &ANONCE, [0; 8], &[], [0; 16]);
        refuse(&mut sup, &pdu, Error::BadKeyInfo(2 | flags));
    }
    // Message 1 with a MIC or a Secure bit.
    for flags in [0x0188, 0x0288] {
        refuse(
            &mut sup,
            &raw_frame(2, flags, 16, 9, &ANONCE, [0; 8], &[], [0; 16]),
            Error::BadKeyInfo(2 | flags),
        );
    }
    assert_eq!(sup.input(&good).unwrap().len(), 4);
}

#[test]
fn out_of_order_messages() {
    let mut sup = supplicant(Akm::Psk);
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    // Message 3 before message 1: nothing to verify it with.
    let ie = auth.rsn_ie.clone();
    refuse(&mut sup, &auth.msg3(&ie, 1, &GTK), Error::Unexpected);
    // A group message before the pairwise key.
    refuse(&mut sup, &auth.group1(1, &GTK), Error::Unexpected);
    // A message 1 that repeats the installed ANonce after completion.
    let (mut sup, mut auth) = established(Akm::Psk);
    refuse(&mut sup, &auth.msg1(), Error::StaleAnonce);
}

#[test]
fn nonce_and_descriptor_checks() {
    let (mut sup, mut auth) = waiting(Akm::Psk);
    let ie = auth.rsn_ie.clone();
    auth.anonce = [0x44; 32]; // sealed under the right PTK, but another ANonce
    refuse(&mut sup, &auth.msg3(&ie, 1, &GTK), Error::AnonceChanged);
    let mut fresh = supplicant(Akm::Psk);
    refuse(
        &mut fresh,
        &raw_frame(2, 0x0088, 16, 1, &[0; 32], [0; 8], &[], [0; 16]),
        Error::ZeroAnonce,
    );
    let mut wpa1 = raw_frame(2, 0x0088, 16, 1, &ANONCE, [0; 8], &[], [0; 16]);
    wpa1[4] = 254;
    refuse(&mut fresh, &wpa1, Error::UnsupportedDescriptor(254));
    // AKM 2 wants version 2; versions 1 (TKIP/RC4) and 3 are refused.
    for version in [1, 3, 0] {
        refuse(
            &mut fresh,
            &raw_frame(version, 0x0088, 16, 1, &ANONCE, [0; 8], &[], [0; 16]),
            Error::BadVersion(version),
        );
    }
    refuse(&mut fresh, &[2, 0, 0, 0], Error::NotKey);
    refuse(&mut fresh, &[], Error::Short);
}

#[test]
fn unsupported_akm_and_ciphers_are_named() {
    let try_new = |edit: &dyn Fn(&mut Config)| {
        let mut cfg = config(Akm::Psk, PMK);
        edit(&mut cfg);
        Supplicant::new(cfg, Standard::new(|| [0; 32])).err()
    };
    assert_eq!(try_new(&|_| {}), None);
    assert_eq!(
        try_new(&|c| c.akm = Akm::Sae),
        Some(Error::UnsupportedAkm(Akm::Sae))
    );
    assert_eq!(
        try_new(&|c| c.akm = Akm::Ieee8021x),
        Some(Error::UnsupportedAkm(Akm::Ieee8021x))
    );
    assert_eq!(
        try_new(&|c| c.akm = Akm::FtPsk),
        Some(Error::UnsupportedAkm(Akm::FtPsk))
    );
    for cipher in [
        Cipher::Tkip,
        Cipher::Wep40,
        Cipher::Wep104,
        Cipher::Gcmp128,
        Cipher::Ccmp256,
    ] {
        assert_eq!(
            try_new(&|c| c.pairwise = cipher),
            Some(Error::UnsupportedCipher(cipher))
        );
        assert_eq!(
            try_new(&|c| c.group = cipher),
            Some(Error::UnsupportedCipher(cipher))
        );
    }
    assert_eq!(
        Error::UnsupportedCipher(Cipher::Tkip).deauth_reason(),
        Some(19)
    );
    assert!(Error::UnsupportedCipher(Cipher::Tkip)
        .message()
        .contains("CCMP"));
}

#[test]
fn the_aps_offer_and_our_association_element_must_agree_with_the_choice() {
    let try_new = |edit: &dyn Fn(&mut Config)| {
        let mut cfg = config(Akm::Psk, PMK);
        edit(&mut cfg);
        Supplicant::new(cfg, Standard::new(|| [0; 32])).err()
    };
    // The AP offers only SAE, or only TKIP, or garbage.
    let sae = Rsn {
        akms: std::vec![Akm::Sae],
        ..Rsn::wpa2_psk()
    }
    .to_ie()
    .unwrap();
    let tkip = Rsn {
        pairwise: std::vec![Cipher::Tkip],
        ..Rsn::wpa2_psk()
    }
    .to_ie()
    .unwrap();
    assert_eq!(
        try_new(&|c| c.ap_rsn_ie = sae.clone()),
        Some(Error::NotOffered)
    );
    assert_eq!(
        try_new(&|c| c.ap_rsn_ie = tkip.clone()),
        Some(Error::NotOffered)
    );
    assert_eq!(
        try_new(&|c| c.ap_rsn_ie = std::vec![48, 1, 1]),
        Some(Error::NotOffered)
    );
    // The AP requires management frame protection.
    let pmf = Rsn {
        caps: RsnCaps(RsnCaps::MFPR | RsnCaps::MFPC),
        ..Rsn::wpa2_psk()
    };
    assert_eq!(
        try_new(&|c| c.ap_rsn_ie = pmf.to_ie().unwrap()),
        Some(Error::PmfRequired)
    );
    // An AP offering several suites is fine when ours is among them.
    let many = Rsn {
        pairwise: std::vec![Cipher::Tkip, Cipher::Ccmp128],
        akms: std::vec![Akm::Sae, Akm::Psk],
        ..Rsn::wpa2_psk()
    };
    assert_eq!(try_new(&|c| c.ap_rsn_ie = many.to_ie().unwrap()), None);
    // Our association element must state exactly one suite each, ours.
    assert_eq!(
        try_new(&|c| c.assoc_rsn_ie = many.to_ie().unwrap()),
        Some(Error::AssocIeMismatch)
    );
    assert_eq!(
        try_new(&|c| c.assoc_rsn_ie = rsn_ie(Akm::PskSha256)),
        Some(Error::AssocIeMismatch)
    );
    assert_eq!(
        try_new(&|c| c.assoc_rsn_ie = std::vec![]),
        Some(Error::AssocIeMismatch)
    );
}

#[test]
fn crypto_trait_is_swappable() {
    // A pinned-nonce implementation proves the trait boundary: a second
    // message 1 with the same ANonce reuses the first SNonce without asking.
    struct Counting(u32);
    impl Crypto for Counting {
        fn random_nonce(&mut self) -> [u8; 32] {
            self.0 += 1;
            [self.0 as u8; 32]
        }
        fn derive_ptk(
            &self,
            a: Akm,
            p: &Key<32>,
            aa: &[u8; 6],
            s: &[u8; 6],
            an: &[u8; 32],
            sn: &[u8; 32],
        ) -> Key<48> {
            Standard::new(|| [0; 32]).derive_ptk(a, p, aa, s, an, sn)
        }
        fn mic(&self, a: Akm, k: &Key<16>, pdu: &[u8]) -> [u8; 16] {
            Standard::new(|| [0; 32]).mic(a, k, pdu)
        }
        fn key_unwrap(&self, k: &Key<16>, w: &[u8]) -> Option<Vec<u8>> {
            Standard::new(|| [0; 32]).key_unwrap(k, w)
        }
    }
    let mut sup = Supplicant::new(config(Akm::Psk, PMK), Counting(0)).unwrap();
    let mut auth = Authenticator::new(Akm::Psk, PMK, ANONCE);
    sup.input(&auth.msg1()).unwrap();
    sup.input(&auth.msg1()).unwrap();
    assert_eq!(sup.crypto().0, 1);
    let mut other = Authenticator::new(Akm::Psk, PMK, [0x66; 32]);
    other.replay = 10;
    sup.input(&other.msg1()).unwrap();
    assert_eq!(sup.crypto().0, 2);
}
