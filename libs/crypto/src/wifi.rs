//! IEEE 802.11 key derivation and key-handshake primitives (WPA2/WPA3-PSK).
//!
//! `docs/wifi-prerequisites-plan.md` section 3.2: the station manager
//! (`wlanmd`) and the supplicant library (`libs/eapol`) need these, `keyd`
//! computes the PMK. Everything here is a thin, length-checked layer over the
//! vetted RustCrypto crates; none of it is home-grown. Inputs come off the
//! air or from a user typing a passphrase, so every function validates its
//! lengths and returns [`Error`] instead of panicking.
//!
//! | Function | Standard | Used for |
//! |---|---|---|
//! | [`pbkdf2_sha1`] | 802.11-2020 J.4 / RFC 8018 | passphrase to PMK |
//! | [`prf_sha1`] | 802.11-2020 12.7.1.2 (PRF-n) | PTK, AKM 2 |
//! | [`kdf_sha256`] | 802.11-2020 12.7.1.7.2 (KDF) | PTK, AKM 6 |
//! | [`hmac_sha1_128`] | 802.11-2020 12.7.2 | EAPOL-Key MIC, AKM 2 |
//! | [`aes_cmac_128`] | RFC 4493 | EAPOL-Key MIC, AKM 6 |
//! | [`aes_wrap`], [`aes_unwrap`] | RFC 3394 | GTK / IGTK in message 3 |

use alloc::vec::Vec;

use aes::Aes128;
use aes_kw::{KekAes128, KekAes256};
use cmac::Cmac;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;

use crate::Error;

/// Length of a WPA2-PSK pairwise master key.
pub const PMK_LEN: usize = 32;
/// Length of an EAPOL-Key MIC for the 128-bit MIC suites.
pub const MIC_LEN: usize = 16;

/// PBKDF2 iteration count fixed by 802.11 (Annex J.4).
const PSK_ROUNDS: u32 = 4096;
/// A WPA passphrase is 8..=63 printable ASCII characters (802.11 Annex M.4).
const PASSPHRASE_LEN: core::ops::RangeInclusive<usize> = 8..=63;
/// An SSID is 1..=32 octets (a zero-length SSID is the wildcard, not a name).
const SSID_LEN: core::ops::RangeInclusive<usize> = 1..=32;
/// The PRF counter is one octet, so at most 256 SHA-1 blocks of output.
const PRF_MAX: usize = 256 * 20;
/// The KDF output length `L` is a 16-bit count of bits.
const KDF_MAX: usize = 0xFFFF / 8;

/// WPA2-PSK: derive the 256-bit PMK from a passphrase and the SSID.
///
/// `PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 256 bits)`. The passphrase must
/// be 8..=63 printable ASCII bytes and the SSID 1..=32 bytes, as the standard
/// requires; anything else is [`Error::BadLength`] (a 64-hex-digit PSK is
/// already a PMK and does not go through here).
pub fn pbkdf2_sha1(passphrase: &[u8], ssid: &[u8]) -> Result<[u8; PMK_LEN], Error> {
    let printable = passphrase.iter().all(|byte| (32..=126).contains(byte));
    if !PASSPHRASE_LEN.contains(&passphrase.len()) || !printable || !SSID_LEN.contains(&ssid.len())
    {
        return Err(Error::BadLength);
    }
    let mut pmk = [0u8; PMK_LEN];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, ssid, PSK_ROUNDS, &mut pmk);
    Ok(pmk)
}

/// IEEE 802.11 PRF-n with SHA-1 (12.7.1.2): fill `out` with
/// `HMAC-SHA1(key, label || 0x00 || data || counter)` blocks, counter from 0.
///
/// `out` may be up to 5120 bytes (one-octet counter); the PTK needs 48 or 64.
pub fn prf_sha1(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) -> Result<(), Error> {
    if out.is_empty() || out.len() > PRF_MAX {
        return Err(Error::BadLength);
    }
    for (counter, chunk) in out.chunks_mut(20).enumerate() {
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).map_err(|_| Error::BadLength)?;
        mac.update(label);
        mac.update(&[0]);
        mac.update(data);
        // `counter < 256` because `out.len() <= PRF_MAX`.
        mac.update(&[counter as u8]);
        chunk.copy_from_slice(&mac.finalize().into_bytes()[..chunk.len()]);
    }
    Ok(())
}

/// IEEE 802.11 KDF with SHA-256 (12.7.1.7.2), used for the PTK of AKM 6:
/// blocks of `HMAC-SHA256(key, i || label || context || L)` where `i` counts
/// from 1 and `i` and `L` (the output length in bits) are 16-bit little endian.
pub fn kdf_sha256(key: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) -> Result<(), Error> {
    if out.is_empty() || out.len() > KDF_MAX {
        return Err(Error::BadLength);
    }
    let bits = (out.len() * 8) as u16;
    for (index, chunk) in out.chunks_mut(32).enumerate() {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| Error::BadLength)?;
        mac.update(&(index as u16 + 1).to_le_bytes());
        mac.update(label);
        mac.update(context);
        mac.update(&bits.to_le_bytes());
        chunk.copy_from_slice(&mac.finalize().into_bytes()[..chunk.len()]);
    }
    Ok(())
}

/// HMAC-SHA1 truncated to 128 bits: the EAPOL-Key MIC of AKM 2 (WPA2-PSK).
///
/// The caller passes the frame with its MIC field zeroed. Compare a received
/// MIC with [`mic_eq`], never `==`.
pub fn hmac_sha1_128(key: &[u8], msg: &[u8]) -> [u8; MIC_LEN] {
    // HMAC accepts any key length, so `new_from_slice` cannot fail.
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    let mut mic = [0u8; MIC_LEN];
    mic.copy_from_slice(&mac.finalize().into_bytes()[..MIC_LEN]);
    mic
}

/// AES-128-CMAC (RFC 4493): the EAPOL-Key MIC of AKM 6 (PSK-SHA256).
pub fn aes_cmac_128(key: &[u8; 16], msg: &[u8]) -> [u8; MIC_LEN] {
    // The key is exactly the AES-128 key size, so `new_from_slice` cannot fail.
    let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key).expect("AES-128 key is 16 bytes");
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

/// Constant-time MIC comparison (no early exit on the first differing byte).
pub fn mic_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let diff = left.iter().zip(right).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    diff == 0
}

/// AES key wrap (RFC 3394) of `plain` under `kek` (16 or 32 bytes). The output
/// is 8 bytes longer. `plain` must be a multiple of 8 bytes, at least 16.
pub fn aes_wrap(kek: &[u8], plain: &[u8]) -> Result<Vec<u8>, Error> {
    if plain.len() < 16 || plain.len() % 8 != 0 {
        return Err(Error::BadLength);
    }
    let wrapped = match kek.len() {
        16 => KekAes128::try_from(kek).map_err(|_| Error::BadLength)?.wrap_vec(plain),
        32 => KekAes256::try_from(kek).map_err(|_| Error::BadLength)?.wrap_vec(plain),
        _ => return Err(Error::BadLength),
    };
    wrapped.map_err(|_| Error::BadLength)
}

/// AES key unwrap (RFC 3394), the receiving side of GTK delivery. `data` is
/// the wrapped key-data field: a multiple of 8 bytes, at least 24. A failed
/// integrity check (wrong KEK or tampering) is [`Error::BadTag`]; the partial
/// plaintext is never returned.
pub fn aes_unwrap(kek: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() < 24 || data.len() % 8 != 0 {
        return Err(Error::BadLength);
    }
    let plain = match kek.len() {
        16 => KekAes128::try_from(kek).map_err(|_| Error::BadLength)?.unwrap_vec(data),
        32 => KekAes256::try_from(kek).map_err(|_| Error::BadLength)?.unwrap_vec(data),
        _ => return Err(Error::BadLength),
    };
    plain.map_err(|_| Error::BadTag)
}

#[cfg(test)]
mod tests;
