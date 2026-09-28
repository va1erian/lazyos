//! Authenticated wrapping of key material (`Wrap`/`Unwrap`).
//!
//! `docs/security-model.md` section 8 gives `keyd` a `Wrap/Unwrap` interface so
//! a client can store, move or persist a secret without ever seeing it in the
//! clear and without ever receiving the wrapping key. This module is the
//! cryptographic half of that contract.
//!
//! # Construction
//!
//! `wrap` is **derive-encrypt-then-MAC** built only from the vetted
//! HKDF-SHA256 (RFC 5869), HMAC-SHA256 (RFC 2104) and SHA-256 (FIPS 180-4)
//! already used elsewhere in this crate:
//!
//! ```text
//! blob   = 0x01 || nonce[16] || ciphertext[len] || tag[32]
//! prk    = HKDF-Extract(salt = nonce, ikm = key)
//! stream = HKDF-Expand(prk, "lazyos-wrap-stream-v1")
//! mac    = HKDF-Expand(prk, "lazyos-wrap-mac-v1")
//! ct     = plaintext XOR PRF(stream, counter)        (HMAC counter mode)
//! tag    = HMAC-SHA256(mac, 0x01 || nonce || len64 || ct)
//! ```
//!
//! `unwrap` recomputes the tag and verifies it in constant time *before*
//! decrypting, so a tampered blob fails closed without touching the plaintext.
//!
//! # Why not an AEAD crate?
//!
//! The target crate would be `chacha20poly1305` (or `aes-gcm`). Both are pure
//! Rust and `no_std`, but their x86 SIMD/asm backends currently fail codegen on
//! the pinned nightly for `x86_64-unknown-none` (LLVM legalizer error in
//! `poly1305`/`polyval`/`aes`; `chacha20` crashes the compiler when
//! instantiated). `sha2` compiles because it is pinned to its `force-soft`
//! backend. The follow-up is to switch `FORMAT_V1`'s body to ChaCha20-Poly1305
//! once those crates build for the target, behind the same interface: the
//! format byte makes the change a version bump, not a breaking API change.
//!
//! This is deliberately *not* a home-grown cipher: the confidentiality comes
//! from HKDF (an extract-and-expand PRF) and the integrity from HMAC, both
//! vetted constructions; only the composition is LazyOS-specific and it is
//! documented here.

use alloc::vec::Vec;

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::hmac::{hmac_sha256_parts, TAG_LEN};
use crate::Error;

/// Format byte for the v1 HKDF+HMAC construction.
pub const FORMAT_V1: u8 = 1;
/// Nonce length: 128 bits from the caller's entropy pool.
pub const NONCE_LEN: usize = 16;
/// Minimum wrapping-key length: 128 bits.
pub const MIN_KEY_LEN: usize = 16;
/// Bytes `wrap` adds to a plaintext.
pub const OVERHEAD: usize = 1 + NONCE_LEN + TAG_LEN;

/// Domain-separation labels for the two subkeys.
const STREAM_INFO: &[u8] = b"lazyos-wrap-stream-v1";
const MAC_INFO: &[u8] = b"lazyos-wrap-mac-v1";

/// The derived `(stream_key, mac_key)` pair for one `(key, nonce)`.
fn derive_subkeys(key: &[u8], nonce: &[u8]) -> ([u8; 32], [u8; 32]) {
    let hkdf = Hkdf::<Sha256>::new(Some(nonce), key);
    let mut stream_key = [0u8; 32];
    let mut mac_key = [0u8; 32];
    // `expand` only fails when the output is longer than 255 * hash length;
    // both requests are exactly one hash long, so the results are infallible.
    hkdf.expand(STREAM_INFO, &mut stream_key)
        .expect("32 bytes is a legal HKDF output");
    hkdf.expand(MAC_INFO, &mut mac_key)
        .expect("32 bytes is a legal HKDF output");
    (stream_key, mac_key)
}

/// XOR `data` with the HMAC counter-mode keystream under `stream_key`.
fn xor_keystream(stream_key: &[u8], data: &mut [u8]) {
    let mut counter = 0u64;
    let mut done = 0usize;
    while done < data.len() {
        let mut block = [0u8; TAG_LEN];
        hmac_sha256_parts(
            stream_key,
            &[b"lazyos-wrap-keystream-v1", &counter.to_le_bytes()],
            &mut block,
        );
        let take = core::cmp::min(block.len(), data.len() - done);
        for (slot, byte) in data[done..done + take].iter_mut().zip(&block[..take]) {
            *slot ^= *byte;
        }
        done += take;
        counter = counter.wrapping_add(1);
    }
}

/// The tag over the framed header and ciphertext.
fn tag(mac_key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> [u8; TAG_LEN] {
    let mut out = [0u8; TAG_LEN];
    hmac_sha256_parts(
        mac_key,
        &[
            &[FORMAT_V1],
            nonce,
            &(ciphertext.len() as u64).to_le_bytes(),
            ciphertext,
        ],
        &mut out,
    );
    out
}

/// Wrap `plaintext` under `key` with caller-supplied `nonce` bytes and return
/// the blob. Deterministic for a fixed `(key, nonce, plaintext)`, which is what
/// the tests and the kernel round-trip check rely on; live callers must pass a
/// fresh nonce from [`crate::rng::Entropy`] (reusing one leaks XOR of the
/// plaintexts, and a repeated `(key, nonce)` pair is detectable).
pub fn wrap_with_nonce(
    key: &[u8],
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, Error> {
    if key.len() < MIN_KEY_LEN {
        return Err(Error::KeyTooShort);
    }
    let (stream_key, mac_key) = derive_subkeys(key, nonce);
    let mut ciphertext = Vec::with_capacity(plaintext.len());
    ciphertext.extend_from_slice(plaintext);
    xor_keystream(&stream_key, &mut ciphertext);

    let mut blob = Vec::with_capacity(OVERHEAD + plaintext.len());
    blob.push(FORMAT_V1);
    blob.extend_from_slice(nonce);
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag(&mac_key, nonce, &ciphertext));
    Ok(blob)
}

/// Unwrap a blob produced by [`wrap_with_nonce`].
///
/// Returns [`Error::Malformed`] when the framing is wrong (short, or an
/// unknown format byte) and [`Error::BadTag`] when the tag does not verify.
/// Both hide *why* a key is wrong from an attacker probing the service.
pub fn unwrap(key: &[u8], blob: &[u8]) -> Result<Vec<u8>, Error> {
    if key.len() < MIN_KEY_LEN {
        return Err(Error::KeyTooShort);
    }
    if blob.len() < OVERHEAD {
        return Err(Error::Malformed);
    }
    if blob[0] != FORMAT_V1 {
        return Err(Error::Malformed);
    }
    let nonce = &blob[1..1 + NONCE_LEN];
    let ciphertext_len = blob.len() - OVERHEAD;
    let ciphertext = &blob[1 + NONCE_LEN..1 + NONCE_LEN + ciphertext_len];
    let expected_tag = &blob[1 + NONCE_LEN + ciphertext_len..];

    let (stream_key, mac_key) = derive_subkeys(key, nonce);
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&mac_key).expect("HMAC accepts any key length");
    mac.update(&[FORMAT_V1]);
    mac.update(nonce);
    mac.update(&(ciphertext.len() as u64).to_le_bytes());
    mac.update(ciphertext);
    // Constant-time comparison; on failure nothing is decrypted.
    mac.verify_slice(expected_tag).map_err(|_| Error::BadTag)?;

    let mut plaintext = Vec::with_capacity(ciphertext_len);
    plaintext.extend_from_slice(ciphertext);
    xor_keystream(&stream_key, &mut plaintext);
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn roundtrip_all_boundary_lengths() {
        for len in [0usize, 1, 15, 16, 17, 31, 32, 33, 255, 1000] {
            let plaintext: Vec<u8> = (0..len).map(|index| (index * 7) as u8).collect();
            let nonce = [len as u8; NONCE_LEN];
            let blob = wrap_with_nonce(KEY, &nonce, &plaintext).unwrap();
            assert_eq!(blob.len(), plaintext.len() + OVERHEAD, "length {len}");
            assert_eq!(blob[0], FORMAT_V1);
            let opened = unwrap(KEY, &blob).unwrap();
            assert_eq!(opened, plaintext, "length {len}");
        }
    }

    #[test]
    fn tampering_any_region_fails_closed() {
        let plaintext = b"the launch codes are in the vault";
        let nonce = [0x5au8; NONCE_LEN];
        let blob = wrap_with_nonce(KEY, &nonce, plaintext).unwrap();

        let mut bad_format = blob.clone();
        bad_format[0] = 0x02;
        assert_eq!(unwrap(KEY, &bad_format), Err(Error::Malformed));

        let mut bad_nonce = blob.clone();
        bad_nonce[1] ^= 1;
        assert_eq!(unwrap(KEY, &bad_nonce), Err(Error::BadTag));

        let mut bad_ciphertext = blob.clone();
        let middle = 1 + NONCE_LEN + plaintext.len() / 2;
        bad_ciphertext[middle] ^= 1;
        assert_eq!(unwrap(KEY, &bad_ciphertext), Err(Error::BadTag));

        let mut bad_tag = blob.clone();
        let last = bad_tag.len() - 1;
        bad_tag[last] ^= 1;
        assert_eq!(unwrap(KEY, &bad_tag), Err(Error::BadTag));

        assert_eq!(unwrap(KEY, &blob[..OVERHEAD - 1]), Err(Error::Malformed));
        assert_eq!(unwrap(b"another-key-0123456789", &blob), Err(Error::BadTag));
        assert_eq!(unwrap(b"short", &blob), Err(Error::KeyTooShort));
    }

    #[test]
    fn fresh_nonce_changes_ciphertext() {
        let plaintext = b"same secret";
        let first = wrap_with_nonce(KEY, &[1u8; NONCE_LEN], plaintext).unwrap();
        let second = wrap_with_nonce(KEY, &[2u8; NONCE_LEN], plaintext).unwrap();
        assert_ne!(first, second);
        let ciphertext_first = &first[1 + NONCE_LEN..first.len() - TAG_LEN];
        let ciphertext_second = &second[1 + NONCE_LEN..second.len() - TAG_LEN];
        assert_ne!(ciphertext_first, ciphertext_second);
        assert_eq!(unwrap(KEY, &first).unwrap(), plaintext);
        assert_eq!(unwrap(KEY, &second).unwrap(), plaintext);
    }

    /// A construction-stability pin: fixed key, nonce and message produce this
    /// exact blob. It fails loudly if the format or derivation changes, which
    /// forces a `FORMAT_V2` bump instead of silently breaking stored blobs.
    #[test]
    fn format_stability_vector() {
        let blob = wrap_with_nonce(KEY, &[0x24u8; NONCE_LEN], b"lazyos").unwrap();
        assert_eq!(
            hex::encode(&blob),
            "01242424242424242424242424242424245bf962cfd1c46f13ebdc21d4c59088d688346378d3932053c9dabbae866ebe3ac6d9a9d3be17"
        );
        assert_eq!(unwrap(KEY, &blob).unwrap(), b"lazyos");
        assert_eq!(blob.len(), OVERHEAD + 6, "vector length changed");
    }
}
