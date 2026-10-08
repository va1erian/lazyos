//! Runs the same primitives `keyd` links in ring 3 inside the kernel
//! (issue #102).

use super::*;
use lazyos_crypto::{hex, hmac, sha256, wrap};

/// Friendly text for a crypto failure.
fn crypto_reason(error: lazyos_crypto::Error) -> String {
    error.message().into()
}

/// FIPS 180-4 and RFC 4231 vectors, run on the freestanding target so the
/// exact artifact `keyd` embeds is covered, not just the host build.
pub fn sha256_hmac_known_answers() -> Result<(), String> {
    let digest = hex::encode(&sha256::sha256(b"abc"));
    check!(
        digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha256(abc) = {digest}"
    );
    let empty = hex::encode(&sha256::sha256(b""));
    check!(
        empty == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256(\"\") = {empty}"
    );
    let tag = hex::encode(&hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There"));
    check!(
        tag == "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "hmac(key, \"Hi There\") = {tag}"
    );
    check!(
        hmac::hmac_sha256_verify(
            &[0x0bu8; 20],
            &[b"Hi There"],
            &hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There")
        ),
        "the constant-time tag check rejected a valid tag"
    );
    Ok(())
}

/// A wrap->unwrap round-trip in kernel context: the wrapper round-trips and
/// refuses tampering.
pub fn keyd_wrap_roundtrip() -> Result<(), String> {
    let key = [0x42u8; 32];
    let nonce = [0x24u8; wrap::NONCE_LEN];
    let secret = b"launch codes: 0000";
    let blob = wrap::wrap_with_nonce(&key, &nonce, secret).map_err(crypto_reason)?;
    let opened = wrap::unwrap(&key, &blob).map_err(crypto_reason)?;
    check!(opened == secret, "wrap round-trip mismatch");
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    check!(
        wrap::unwrap(&key, &tampered) == Err(lazyos_crypto::Error::BadTag),
        "a tampered blob unwrapped"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("keyd_sha256_hmac_known_answers", sha256_hmac_known_answers),
    ("keyd_wrap_roundtrip", keyd_wrap_roundtrip),
];
