//! The cryptography the supplicant needs, behind a trait so tests and the
//! fuzz target can pin nonces and stand in for the real functions.
//!
//! [`Standard`] is the real thing: the WP0 functions of `lazyos-crypto`
//! (`prf_sha1`, `kdf_sha256`, `hmac_sha1_128`, `aes_cmac_128`, `aes_unwrap`)
//! plus a nonce source the caller supplies (the kernel's CSPRNG in `wlanmd`).

use alloc::vec::Vec;

use ieee80211::Akm;
use lazyos_crypto::wifi;

use crate::secret::Key;

/// PTK label (12.7.1.3).
const PTK_LABEL: &[u8] = b"Pairwise key expansion";

/// Octets of KCK + KEK + TK for CCMP-128.
pub const PTK_LEN: usize = 48;

/// The operations of 12.7: PTK derivation, EAPOL-Key MIC, key unwrap, nonce.
///
/// Only AKM 2 and 6 reach an implementation (the supplicant refuses the rest
/// before it asks).
pub trait Crypto {
    /// A fresh 32-octet SNonce. Must be unpredictable (12.7.5).
    fn random_nonce(&mut self) -> [u8; 32];

    /// `PTK = PRF/KDF-384(PMK, "Pairwise key expansion", min(AA,SPA) ||
    /// max(AA,SPA) || min(ANonce,SNonce) || max(ANonce,SNonce))`.
    fn derive_ptk(
        &self,
        akm: Akm,
        pmk: &Key<32>,
        aa: &[u8; 6],
        spa: &[u8; 6],
        anonce: &[u8; 32],
        snonce: &[u8; 32],
    ) -> Key<PTK_LEN>;

    /// The EAPOL-Key MIC of `pdu` (whose MIC field is zero) under `kck`.
    fn mic(&self, akm: Akm, kck: &Key<16>, pdu: &[u8]) -> [u8; 16];

    /// RFC 3394 unwrap under `kek`; `None` if the integrity check fails.
    fn key_unwrap(&self, kek: &Key<16>, wrapped: &[u8]) -> Option<Vec<u8>>;
}

/// The data the PTK is derived over: sorted addresses, then sorted nonces.
pub fn ptk_data(aa: &[u8; 6], spa: &[u8; 6], anonce: &[u8; 32], snonce: &[u8; 32]) -> Vec<u8> {
    let mut data = Vec::with_capacity(76);
    let (lo, hi) = if aa <= spa { (aa, spa) } else { (spa, aa) };
    data.extend_from_slice(lo);
    data.extend_from_slice(hi);
    let (lo, hi) = if anonce <= snonce {
        (anonce, snonce)
    } else {
        (snonce, anonce)
    };
    data.extend_from_slice(lo);
    data.extend_from_slice(hi);
    data
}

/// The real implementation over `lazyos-crypto`, with the nonce from `nonce`.
pub struct Standard<N: FnMut() -> [u8; 32]> {
    nonce: N,
}

impl<N: FnMut() -> [u8; 32]> Standard<N> {
    pub fn new(nonce: N) -> Standard<N> {
        Standard { nonce }
    }
}

impl<N: FnMut() -> [u8; 32]> Crypto for Standard<N> {
    fn random_nonce(&mut self) -> [u8; 32] {
        (self.nonce)()
    }

    fn derive_ptk(
        &self,
        akm: Akm,
        pmk: &Key<32>,
        aa: &[u8; 6],
        spa: &[u8; 6],
        anonce: &[u8; 32],
        snonce: &[u8; 32],
    ) -> Key<PTK_LEN> {
        let data = ptk_data(aa, spa, anonce, snonce);
        let mut ptk = Key::new([0; PTK_LEN]);
        // The output length (48) and the key are always valid for these two
        // functions, so the Result carries no information here.
        let _ = match akm {
            Akm::PskSha256 => wifi::kdf_sha256(pmk.expose(), PTK_LABEL, &data, ptk.expose_mut()),
            _ => wifi::prf_sha1(pmk.expose(), PTK_LABEL, &data, ptk.expose_mut()),
        };
        ptk
    }

    fn mic(&self, akm: Akm, kck: &Key<16>, pdu: &[u8]) -> [u8; 16] {
        match akm {
            Akm::PskSha256 => wifi::aes_cmac_128(kck.expose(), pdu),
            _ => wifi::hmac_sha1_128(kck.expose(), pdu),
        }
    }

    fn key_unwrap(&self, kek: &Key<16>, wrapped: &[u8]) -> Option<Vec<u8>> {
        wifi::aes_unwrap(kek.expose(), wrapped).ok()
    }
}
