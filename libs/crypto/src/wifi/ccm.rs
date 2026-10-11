//! AES-128-CCM as CCMP-128 uses it (IEEE 802.11-2020 12.5.3): an 8-octet
//! MIC (`M = 8`), a two-octet length field (`L = 2`) and so a 13-octet nonce.
//!
//! A Wi-Fi chip runs this in hardware; the simulated radio (`wifisim`, CI
//! only) runs it here, so a wrong temporal key or packet number is caught by
//! the access point on the other side exactly as real hardware would make it.
//! The nonce and AAD come from `ieee80211::ccmp::framing`. A thin layer over
//! RustCrypto's `ccm` crate: nothing here is home-grown.

use aes::Aes128;
use ccm::aead::generic_array::GenericArray;
use ccm::aead::{AeadInPlace, KeyInit};
use ccm::consts::{U13, U8};
use ccm::Ccm;

use crate::Error;

/// The CCMP-128 temporal key.
pub const TK_LEN: usize = 16;
/// The CCMP-128 MIC.
pub const MIC_LEN: usize = 8;
/// The CCMP nonce.
pub const NONCE_LEN: usize = 13;

type Ccmp128 = Ccm<Aes128, U8, U13>;

/// Encrypt `data` in place under `tk` and return the MIC that follows it on
/// the air. `aad` is the frame's masked header. Fails only for a payload too
/// long for a two-octet length field ([`Error::BadLength`]).
pub fn ccm_seal(
    tk: &[u8; TK_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    data: &mut [u8],
) -> Result<[u8; MIC_LEN], Error> {
    let cipher = Ccmp128::new(GenericArray::from_slice(tk));
    let tag = cipher
        .encrypt_in_place_detached(GenericArray::from_slice(nonce), aad, data)
        .map_err(|_| Error::BadLength)?;
    let mut mic = [0u8; MIC_LEN];
    mic.copy_from_slice(&tag);
    Ok(mic)
}

/// Verify `mic` and decrypt `data` in place. On [`Error::BadTag`] (a wrong
/// key, a wrong nonce, so a wrong PN or address, or tampering) `data` is left
/// as it was received, never partly decrypted.
pub fn ccm_open(
    tk: &[u8; TK_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    data: &mut [u8],
    mic: &[u8; MIC_LEN],
) -> Result<(), Error> {
    let cipher = Ccmp128::new(GenericArray::from_slice(tk));
    // The crate decrypts before it compares the MIC and leaves the buffer
    // decrypted on a mismatch: work on a copy so a refused frame is untouched.
    let mut plain = alloc::vec::Vec::from(&*data);
    cipher
        .decrypt_in_place_detached(
            GenericArray::from_slice(nonce),
            aad,
            &mut plain,
            GenericArray::from_slice(mic),
        )
        .map_err(|_| Error::BadTag)?;
    data.copy_from_slice(&plain);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> alloc::vec::Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// IEEE 802.11-2020 Annex J.6.4, the CCMP test MPDU (TK, nonce and the
    /// muted header as AAD, plaintext, ciphertext and MIC).
    #[test]
    fn annex_j_ccmp_vector() {
        let tk: [u8; 16] = hex("c97c1f67ce371185514a8a19f2bdd52f").try_into().unwrap();
        let nonce: [u8; 13] = hex("005030f1844408b5039776e70c").try_into().unwrap();
        let aad = hex("08400fd2e128a57c5030f1844408abaea5b8fcba0000");
        let mut data = hex("f8ba1a55d02f85ae967bb62fb6cda8eb7e78a050");
        let mic = ccm_seal(&tk, &nonce, &aad, &mut data).unwrap();
        assert_eq!(data, hex("f3d0a2fe9a3dbf2342a643e43246e80c3c04d019"));
        assert_eq!(mic.to_vec(), hex("7845ce0b16f97623"));
        ccm_open(&tk, &nonce, &aad, &mut data, &mic).unwrap();
        assert_eq!(data, hex("f8ba1a55d02f85ae967bb62fb6cda8eb7e78a050"));
    }

    #[test]
    fn wrong_key_nonce_aad_or_mic_is_refused_and_leaves_the_data() {
        let tk = [7u8; 16];
        let nonce = [1u8; 13];
        let mut data = *b"twenty octets of it!";
        let mic = ccm_seal(&tk, &nonce, b"hdr", &mut data).unwrap();
        let sealed = data;
        let mut other = nonce;
        other[12] ^= 1;
        let mut bad_mic = mic;
        bad_mic[0] ^= 0x80;
        for (key, n, aad, m) in [
            ([8u8; 16], nonce, &b"hdr"[..], mic),
            (tk, other, &b"hdr"[..], mic),
            (tk, nonce, &b"hdR"[..], mic),
            (tk, nonce, &b"hdr"[..], bad_mic),
        ] {
            let mut copy = sealed;
            assert_eq!(ccm_open(&key, &n, aad, &mut copy, &m), Err(Error::BadTag));
            assert_eq!(copy, sealed);
        }
        ccm_open(&tk, &nonce, b"hdr", &mut data, &mic).unwrap();
        assert_eq!(&data, b"twenty octets of it!");
    }

    #[test]
    fn empty_payload_still_authenticates_the_header() {
        let mic = ccm_seal(&[1; 16], &[2; 13], b"aad", &mut []).unwrap();
        assert_eq!(ccm_open(&[1; 16], &[2; 13], b"aad", &mut [], &mic), Ok(()));
        assert_eq!(
            ccm_open(&[1; 16], &[2; 13], b"aaD", &mut [], &mic),
            Err(Error::BadTag)
        );
    }
}
