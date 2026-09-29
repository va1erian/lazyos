//! The boot self-test: known-answer vectors and a wrap/verify round-trip.

use super::state::Keyd;
use alloc::format;
use alloc::string::String;
use lazyos_crypto::{hex, hmac, sha256};
use user::messenger::keyd as wire;

/// The boot self-test: known-answer vectors, a wrap round-trip with tamper
/// rejection, password verification, and the RNG. Returns a printable detail
/// on failure so the serial marker says exactly what broke.
pub(crate) fn self_test(keyd: &mut Keyd) -> Result<(), String> {
    // FIPS 180-4: SHA-256("abc").
    let digest = sha256::sha256(b"abc");
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    if hex::encode(&digest) != expected {
        return Err(format!("sha256(abc)={}", hex::encode(&digest)));
    }

    // RFC 4231 case 1: HMAC-SHA256.
    let tag = hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There");
    let expected = "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7";
    if hex::encode(&tag) != expected {
        return Err(format!("hmac={}", hex::encode(&tag)));
    }

    // A generated wrapping key round-trips, and a tampered blob is refused.
    let wrap_key = keyd
        .generate(wire::KIND_WRAP, SELF_TEST_OWNER)
        .ok_or_else(|| String::from("generate wrap key"))?;
    let blob = keyd
        .wrap(wrap_key, SELF_TEST_OWNER, b"selftest secret")
        .map_err(|error| error.message())?;
    let opened = keyd
        .unwrap(wrap_key, SELF_TEST_OWNER, &blob)
        .map_err(|error| error.message())?;
    if opened != b"selftest secret" {
        return Err(String::from("wrap round-trip mismatch"));
    }
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    if keyd.unwrap(wrap_key, SELF_TEST_OWNER, &tampered).is_ok() {
        return Err(String::from("tampered blob unwrapped"));
    }
    // Another uid must not be able to use (or even see) the key.
    let stranger = SELF_TEST_OWNER + 1;
    if keyd.unwrap(wrap_key, stranger, &blob).is_ok()
        || keyd.wrap(wrap_key, stranger, b"x").is_ok()
        || keyd.sign(wrap_key, stranger, b"x").is_ok()
        || !keyd.keys(stranger).is_empty()
    {
        return Err(String::from("a non-owner used or listed a key"));
    }

    // Argon2id password verification, both directions.
    if !keyd.verify("lazyos", "lazyos") {
        return Err(String::from("demo account rejected"));
    }
    if keyd.verify("lazyos", "not-lazyos") {
        return Err(String::from("wrong password accepted"));
    }
    // A provisioned account verifies, and re-provisioning replaces the secret.
    keyd.provision("selftest-user", "first")
        .map_err(|error| error.message())?;
    keyd.provision("selftest-user", "second")
        .map_err(|error| error.message())?;
    if keyd.verify("selftest-user", "first") || !keyd.verify("selftest-user", "second") {
        return Err(String::from(
            "provisioned secret did not replace the old one",
        ));
    }

    // A signing key produces a tag and counts its use.
    let hmac_key = keyd
        .generate(wire::KIND_HMAC, SELF_TEST_OWNER)
        .ok_or_else(|| String::from("generate hmac key"))?;
    let tag = keyd
        .sign(hmac_key, SELF_TEST_OWNER, b"digest")
        .map_err(|error| error.message())?;
    if tag == [0u8; hmac::TAG_LEN] {
        return Err(String::from("sign returned a zero tag"));
    }

    // The pool produces non-trivial bytes.
    let random = keyd.random(32).map_err(|error| error.message())?;
    if random.len() != 32 || random.iter().all(|byte| *byte == 0) {
        return Err(String::from("random output looks wrong"));
    }
    Ok(())
}

/// The owner used by the boot self-test (root; no Messenger sender).
pub(crate) const SELF_TEST_OWNER: u32 = 0;
