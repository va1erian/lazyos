//! A handshake recorded by someone else: Wireshark's `wpa-Induction.pcap`
//! (SSID "Coherer", passphrase "Induction"), through the library's own code
//! paths. See `capture.rs` for the source, SHA-256 and licence.
//!
//! What this capture is: WPA2 descriptor (2), key descriptor version 2
//! (HMAC-SHA1-128), AKM 2 (PSK), CCMP-128 pairwise, but a **TKIP group
//! cipher**, and the client puts 16 in the Key Length of messages 2 and 4
//! (this supplicant sends 0, as 12.7.6 allows for RSN). So the supplicant
//! itself cannot reproduce messages 2 and 4 byte for byte, and it must not
//! try: it refuses a TKIP group cipher. What is verified here is the PMK, the
//! PTK, all three MICs, the key unwrap and the key data of message 3, the
//! frame parsers/encoders on the real bytes, and that the supplicant refuses
//! this network by name rather than accepting it.

use std::vec;
use std::vec::Vec;

use ieee80211::{Akm, Cipher, Rsn};
use lazyos_crypto::wifi;

use super::capture::*;
use super::{unhex, unhex_array};
use crate::{keydata, Config, Crypto, Error, Key, KeyFrame, Standard, Supplicant};

struct Exchange {
    frames: [KeyFrame; 4],
    pdus: [Vec<u8>; 4],
    ptk: Key<48>,
}

fn exchange() -> Exchange {
    let pdus = [unhex(MSG1), unhex(MSG2), unhex(MSG3), unhex(MSG4)];
    let frames = pdus.clone().map(|p| KeyFrame::parse(&p).unwrap());
    let pmk = wifi::pbkdf2_sha1(PASSPHRASE.as_bytes(), SSID.as_bytes()).unwrap();
    let ptk = Standard::new(|| [0; 32]).derive_ptk(
        Akm::Psk,
        &Key::new(pmk),
        &unhex_array(AP),
        &unhex_array(STA),
        &frames[0].nonce,
        &frames[1].nonce,
    );
    Exchange { frames, pdus, ptk }
}

#[test]
fn the_frames_parse_and_encode_to_their_captured_bytes() {
    let e = exchange();
    for (frame, pdu) in e.frames.iter().zip(&e.pdus) {
        assert_eq!(&frame.encode().unwrap(), pdu);
        assert_eq!((frame.descriptor, frame.version()), (2, 2));
    }
    // Message 1 carries the AP's ANonce and a PMKID KDE; 2 the SNonce; 4 none.
    assert_eq!(e.frames[0].flags(), 0x0088);
    assert_eq!(e.frames[1].flags(), 0x0108);
    assert_eq!(e.frames[2].flags(), 0x13C8);
    assert_eq!(e.frames[3].flags(), 0x0308);
    assert_eq!(
        e.frames[2].nonce, e.frames[0].nonce,
        "message 3 repeats the ANonce"
    );
}

#[test]
fn every_mic_in_the_real_exchange_verifies_under_the_derived_ptk() {
    let e = exchange();
    let crypto = Standard::new(|| [0; 32]);
    let kck = Key::<16>::slice_of(&e.ptk, 0);
    for (n, frame) in e.frames.iter().enumerate().skip(1) {
        let expected = crypto.mic(Akm::Psk, &kck, &frame.mic_input().unwrap());
        assert!(wifi::mic_eq(&expected, &frame.mic), "message {} MIC", n + 1);
    }
    // And the MIC is under this exact key: the wrong passphrase fails.
    let wrong = wifi::pbkdf2_sha1(b"Induction!", SSID.as_bytes()).unwrap();
    let ptk = crypto.derive_ptk(
        Akm::Psk,
        &Key::new(wrong),
        &unhex_array(AP),
        &unhex_array(STA),
        &e.frames[0].nonce,
        &e.frames[1].nonce,
    );
    let kck = Key::<16>::slice_of(&ptk, 0);
    let expected = crypto.mic(Akm::Psk, &kck, &e.frames[1].mic_input().unwrap());
    assert!(!wifi::mic_eq(&expected, &e.frames[1].mic));
}

#[test]
fn message_3_unwraps_and_its_key_data_walks() {
    let e = exchange();
    let crypto = Standard::new(|| [0; 32]);
    let kek = Key::<16>::slice_of(&e.ptk, 16);
    let plain = crypto
        .key_unwrap(&kek, &e.frames[2].key_data)
        .expect("the AES key wrap checks");
    let data = keydata::parse(&plain).unwrap();
    // The RSN element is the AP's full offer (TKIP and CCMP pairwise); the
    // client's message 2 narrowed it to CCMP, so the two differ, as they do
    // between the beacon and the association request on a mixed-mode AP.
    let offer = data.rsn_ie.unwrap();
    assert_ne!(offer, &e.frames[1].key_data[..]);
    let rsn = Rsn::parse_ie(offer).unwrap();
    assert_eq!(
        (rsn.group, rsn.pairwise, rsn.akms),
        (
            Cipher::Tkip,
            vec![Cipher::Ccmp128, Cipher::Tkip],
            vec![Akm::Psk]
        )
    );
    let chosen = Rsn::parse_ie(&e.frames[1].key_data).unwrap();
    assert_eq!(
        (chosen.group, chosen.pairwise),
        (Cipher::Tkip, vec![Cipher::Ccmp128])
    );
    // A TKIP group key: 32 octets, which a CCMP-only GTK check refuses.
    let gtk = data.gtk.expect("a GTK KDE");
    assert_eq!(gtk.key.len(), 32);
}

#[test]
fn the_supplicant_refuses_this_networks_tkip_group_cipher_by_name() {
    let e = exchange();
    // The AP's offer is inside message 3; the client's choice is message 2's.
    let plain = Standard::new(|| [0; 32])
        .key_unwrap(&Key::<16>::slice_of(&e.ptk, 16), &e.frames[2].key_data)
        .unwrap();
    let offer = keydata::parse(&plain).unwrap().rsn_ie.unwrap().to_vec();
    let choice = e.frames[1].key_data.clone();
    let config = |group: Cipher| Config {
        akm: Akm::Psk,
        pairwise: Cipher::Ccmp128,
        group,
        pmk: Key::new([0; 32]),
        aa: unhex_array(AP),
        spa: unhex_array(STA),
        ap_rsn_ie: offer.clone(),
        assoc_rsn_ie: choice.clone(),
    };
    let new = |group| Supplicant::new(config(group), Standard::new(|| [0; 32])).err();
    assert_eq!(
        new(Cipher::Tkip),
        Some(Error::UnsupportedCipher(Cipher::Tkip))
    );
    // Choosing CCMP for the group does not help: the AP's element says TKIP.
    assert_eq!(new(Cipher::Ccmp128), Some(Error::NotOffered));
}
