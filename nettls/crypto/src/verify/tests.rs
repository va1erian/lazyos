//! Known-answer tests: signatures made by OpenSSL (`testdata/`, public keys
//! and signatures only) must verify, and every tampered or out-of-policy
//! input must not.

use super::*;

const MSG: &[u8] = include_bytes!("testdata/msg.bin");

fn check(alg: &dyn SignatureVerificationAlgorithm, key: &[u8], sig: &[u8]) {
    alg.verify_signature(key, MSG, sig)
        .expect("valid signature");
    assert!(
        alg.verify_signature(key, b"another message", sig).is_err(),
        "wrong message"
    );
    let mut bad = sig.to_vec();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(
        alg.verify_signature(key, MSG, &bad).is_err(),
        "tampered signature"
    );
}

#[test]
fn ecdsa() {
    check(
        &ECDSA_P256_SHA256,
        include_bytes!("testdata/p256.pub.bin"),
        include_bytes!("testdata/p256-sha256.sig"),
    );
    check(
        &ECDSA_P384_SHA384,
        include_bytes!("testdata/p384.pub.bin"),
        include_bytes!("testdata/p384-sha384.sig"),
    );
    // A P-256 signature checked as P-384, and a point off the curve.
    assert!(ECDSA_P384_SHA256
        .verify_signature(
            include_bytes!("testdata/p256.pub.bin"),
            MSG,
            include_bytes!("testdata/p256-sha256.sig")
        )
        .is_err());
    assert!(ECDSA_P256_SHA256
        .verify_signature(&[4u8; 65], MSG, include_bytes!("testdata/p256-sha256.sig"))
        .is_err());
}

#[test]
fn ed25519() {
    check(
        &ED25519,
        include_bytes!("testdata/ed25519.pub.bin"),
        include_bytes!("testdata/ed25519.sig"),
    );
}

#[test]
fn rsa_pkcs1_and_pss() {
    let key = include_bytes!("testdata/rsa2048.pub.der");
    check(
        &RSA_PKCS1_SHA256,
        key,
        include_bytes!("testdata/rsa2048.pkcs1-sha256.sig"),
    );
    check(
        &RSA_PSS_SHA384,
        key,
        include_bytes!("testdata/rsa2048.pss-sha384.sig"),
    );
    // Same key, wrong padding or hash.
    assert!(RSA_PSS_SHA256
        .verify_signature(
            key,
            MSG,
            include_bytes!("testdata/rsa2048.pkcs1-sha256.sig")
        )
        .is_err());
    assert!(RSA_PKCS1_SHA384
        .verify_signature(
            key,
            MSG,
            include_bytes!("testdata/rsa2048.pkcs1-sha256.sig")
        )
        .is_err());
}

#[test]
fn small_rsa_keys_are_refused() {
    // A valid signature from a 1024-bit key: below the 2048-bit floor.
    let key = include_bytes!("testdata/rsa1024.pub.der");
    let sig = include_bytes!("testdata/rsa1024.pkcs1-sha256.sig");
    assert!(RSA_PKCS1_SHA256.verify_signature(key, MSG, sig).is_err());
    assert!(rsa_key(key).is_err());
}

#[test]
fn every_scheme_maps_to_known_algorithms() {
    for (scheme, algs) in ALGORITHMS.mapping {
        assert!(!algs.is_empty(), "{scheme:?}");
    }
    assert!(ALGORITHMS
        .supported_schemes()
        .contains(&SignatureScheme::RSA_PSS_SHA256));
}
