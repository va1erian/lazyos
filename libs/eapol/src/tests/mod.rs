//! Host tests: frame parsing, key data walking, handshakes against a test
//! authenticator, an independent Python transcript, and the refusals.

use std::vec::Vec;

use ieee80211::{Akm, Cipher, Rsn};

use crate::fuzz::{script_supplicant, SCRIPT_MIC};
use crate::{Config, Key, Standard, Supplicant};

mod auth;
mod capture;
mod frames;
mod negative;
mod real_capture;
mod transcript;
mod vectors;

pub(crate) const AA: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
pub(crate) const SPA: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];

pub(crate) fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

pub(crate) fn unhex_array<const N: usize>(text: &str) -> [u8; N] {
    unhex(text).try_into().unwrap()
}

pub(crate) fn rsn_ie(akm: Akm) -> Vec<u8> {
    match akm {
        Akm::PskSha256 => Rsn::wpa2_psk_sha256(),
        _ => Rsn::wpa2_psk(),
    }
    .to_ie()
    .unwrap()
}

pub(crate) fn config(akm: Akm, pmk: [u8; 32]) -> Config {
    Config {
        akm,
        pairwise: Cipher::Ccmp128,
        group: Cipher::Ccmp128,
        pmk: Key::new(pmk),
        aa: AA,
        spa: SPA,
        ap_rsn_ie: rsn_ie(akm),
        assoc_rsn_ie: rsn_ie(akm),
    }
}

/// A supplicant on the real crypto with the SNonce pinned.
pub(crate) fn real(
    akm: Akm,
    pmk: [u8; 32],
    snonce: [u8; 32],
) -> Supplicant<Standard<impl FnMut() -> [u8; 32]>> {
    Supplicant::new(config(akm, pmk), Standard::new(move || snonce)).unwrap()
}

/// Build an EAPOL-Key PDU octet by octet, independent of `KeyFrame`.
/// `mic` is the 16 octets to place in the MIC field.
#[allow(clippy::too_many_arguments)]
pub(crate) fn raw_frame(
    version: u8,
    flags: u16,
    key_len: u16,
    replay: u64,
    nonce: &[u8; 32],
    rsc: [u8; 8],
    key_data: &[u8],
    mic: [u8; 16],
) -> Vec<u8> {
    let mut f = std::vec![0u8; 99];
    f[0] = 2;
    f[1] = 3;
    f[2..4].copy_from_slice(&((95 + key_data.len()) as u16).to_be_bytes());
    f[4] = 2;
    f[5..7].copy_from_slice(&(u16::from(version) | flags).to_be_bytes());
    f[7..9].copy_from_slice(&key_len.to_be_bytes());
    f[9..17].copy_from_slice(&replay.to_be_bytes());
    f[17..49].copy_from_slice(nonce);
    f[65..73].copy_from_slice(&rsc);
    f[81..97].copy_from_slice(&mic);
    f[97..99].copy_from_slice(&(key_data.len() as u16).to_be_bytes());
    f.extend_from_slice(key_data);
    f
}

/// The key data plaintext of message 3: RSN element, GTK KDE, padding.
pub(crate) fn msg3_plain(rsn_ie: &[u8], index: u8, gtk: &[u8]) -> Vec<u8> {
    let mut p = rsn_ie.to_vec();
    p.extend_from_slice(&[0xDD, 6 + gtk.len() as u8, 0x00, 0x0F, 0xAC, 1, index & 3, 0]);
    p.extend_from_slice(gtk);
    pad(p)
}

/// Pad to a multiple of 8 octets (at least 16) with `dd 00 ...`.
pub(crate) fn pad(mut p: Vec<u8>) -> Vec<u8> {
    if !p.len().is_multiple_of(8) || p.len() < 16 {
        p.push(0xDD);
        while !p.len().is_multiple_of(8) || p.len() < 16 {
            p.push(0);
        }
    }
    p
}

/// A valid exchange under [`crate::fuzz::ScriptCrypto`] in the fuzz script
/// grammar: AKM byte, then `u16` length and PDU for message 1, message 3 and
/// group message 1. (The Python seed generator builds the same bytes.)
pub(crate) fn scripted_handshake(psk_sha256: bool) -> Vec<u8> {
    let version = if psk_sha256 { 3 } else { 2 };
    let akm = if psk_sha256 { Akm::PskSha256 } else { Akm::Psk };
    let mut script = std::vec![u8::from(psk_sha256)];
    let mut push = |pdu: Vec<u8>| {
        script.extend_from_slice(&(pdu.len() as u16).to_be_bytes());
        script.extend_from_slice(&pdu);
    };
    let anonce = [0x11; 32];
    push(raw_frame(
        version,
        0x0088,
        16,
        1,
        &anonce,
        [0; 8],
        &[],
        [0; 16],
    ));
    let mut wrapped = std::vec![0u8; 8];
    wrapped.extend(msg3_plain(&rsn_ie(akm), 1, &[0x22; 16]));
    push(raw_frame(
        version, 0x13C8, 16, 2, &anonce, [1; 8], &wrapped, SCRIPT_MIC,
    ));
    let mut group = std::vec![0u8; 8];
    group.extend(pad([
        &[0xDD, 22, 0x00, 0x0F, 0xAC, 1, 2, 0][..],
        &[0x33; 16],
    ]
    .concat()));
    push(raw_frame(
        version,
        0x1380,
        16,
        3,
        &[0x44; 32],
        [2; 8],
        &group,
        SCRIPT_MIC,
    ));
    script
}

#[test]
fn the_scripted_handshake_is_accepted_end_to_end() {
    for sha256 in [false, true] {
        let script = scripted_handshake(sha256);
        let mut sup = script_supplicant(sha256);
        let mut rest = &script[1..];
        let mut outcomes = Vec::new();
        while let Some((len, tail)) = rest.split_first_chunk::<2>() {
            let (pdu, tail) = tail.split_at(usize::from(u16::from_be_bytes(*len)));
            rest = tail;
            outcomes.push(sup.input(pdu).map(|a| a.len()));
        }
        assert_eq!(outcomes, [Ok(1), Ok(4), Ok(2)], "sha256={sha256}");
    }
}

/// The checked-in fuzz seeds (built by `fuzz/seeds_wifi.py`) hold the very
/// handshake this module builds, so the seeds reach the deep paths.
#[test]
fn fuzz_seeds_match_the_scripted_handshake() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/eapol");
    for (name, sha256) in [("psk_handshake", false), ("sha256_handshake", true)] {
        let seed = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(seed, scripted_handshake(sha256), "{name}");
    }
}
