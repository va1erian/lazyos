//! Known-answer and refusal tests for the Wi-Fi primitives.
//!
//! Each vector names its source. Where a vector is not printed in a standard
//! (the 802.11 KDF has no published one), it was cross-checked against an
//! independent Python implementation (`hashlib`/`hmac`) written from the
//! standard's text, and says so.

use super::*;
use crate::hex::encode;

fn h(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

// --- PBKDF2 / PSK -> PMK ----------------------------------------------------

#[test]
fn psk_ieee_annex_j4_test_case_1() {
    // IEEE 802.11-2020 Annex J.4, passphrase "password", SSID "IEEE".
    let pmk = pbkdf2_sha1(b"password", b"IEEE").unwrap();
    assert_eq!(
        encode(&pmk),
        "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
    );
}

#[test]
fn psk_ieee_annex_j4_test_case_2() {
    // IEEE 802.11-2020 Annex J.4, "ThisIsAPassword" / "ThisIsASSID".
    let pmk = pbkdf2_sha1(b"ThisIsAPassword", b"ThisIsASSID").unwrap();
    assert_eq!(
        encode(&pmk),
        "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af"
    );
}

#[test]
fn psk_matches_rfc6070_prefix() {
    // RFC 6070 section 2: PBKDF2-HMAC-SHA1("password", "salt", 4096, 20) is
    // the first 20 bytes of our 32-byte output (PBKDF2 blocks are independent).
    let pmk = pbkdf2_sha1(b"password", b"salt").unwrap();
    assert_eq!(
        encode(&pmk[..20]),
        "4b007901b765489abead49d926f721d065a429c1"
    );
}

#[test]
fn psk_refuses_bad_passphrase_and_ssid() {
    assert_eq!(pbkdf2_sha1(b"short", b"net"), Err(Error::BadLength));
    assert_eq!(pbkdf2_sha1(&[b'a'; 64], b"net"), Err(Error::BadLength));
    assert_eq!(
        pbkdf2_sha1(b"has\x00control", b"net"),
        Err(Error::BadLength)
    );
    assert_eq!(
        pbkdf2_sha1("pässword1".as_bytes(), b"net"),
        Err(Error::BadLength)
    );
    assert_eq!(pbkdf2_sha1(b"password", b""), Err(Error::BadLength));
    assert_eq!(pbkdf2_sha1(b"password", &[b's'; 33]), Err(Error::BadLength));
    // The bounds themselves are legal.
    assert!(pbkdf2_sha1(&[b'a'; 63], &[b's'; 32]).is_ok());
    assert!(pbkdf2_sha1(b"12345678", b"x").is_ok());
}

// --- PRF-n (SHA-1) ----------------------------------------------------------

#[test]
fn prf_sha1_ieee_annex_j3_vectors() {
    // IEEE 802.11 Annex J.3 (PRF test cases, also hostap's `sha1-prf` tests).
    // Case 1: key 0x0b * 20, "prefix", "Hi There", 192 bits.
    let mut out = [0u8; 24];
    prf_sha1(&[0x0b; 20], b"prefix", b"Hi There", &mut out).unwrap();
    assert_eq!(
        encode(&out),
        "bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606"
    );
    // Case 2: key "Jefe", "prefix-2", "what do ya want for nothing?".
    prf_sha1(
        b"Jefe",
        b"prefix-2",
        b"what do ya want for nothing?",
        &mut out,
    )
    .unwrap();
    assert_eq!(
        encode(&out),
        "47c4908e30c947521ad20be9053450ecbea23d3aa604b773"
    );
}

#[test]
fn prf_sha1_output_is_a_prefix_family() {
    // A shorter request is a prefix of a longer one (the counter is per block).
    let mut long = [0u8; 64];
    let mut short = [0u8; 48];
    prf_sha1(b"key", b"Pairwise key expansion", b"data", &mut long).unwrap();
    prf_sha1(b"key", b"Pairwise key expansion", b"data", &mut short).unwrap();
    assert_eq!(long[..48], short);
}

#[test]
fn prf_sha1_refuses_bad_output_sizes() {
    assert_eq!(prf_sha1(b"k", b"l", b"d", &mut []), Err(Error::BadLength));
    let mut too_long = alloc::vec![0u8; PRF_MAX + 1];
    assert_eq!(
        prf_sha1(b"k", b"l", b"d", &mut too_long),
        Err(Error::BadLength)
    );
    let mut max = alloc::vec![0u8; PRF_MAX];
    assert!(prf_sha1(b"k", b"l", b"d", &mut max).is_ok());
}

// --- KDF (SHA-256) ----------------------------------------------------------

#[test]
fn kdf_sha256_cross_checked_vectors() {
    // No published 802.11 KDF vector; both were produced by an independent
    // Python implementation of 12.7.1.7.2 (hashlib/hmac, counter and length as
    // 16-bit little endian) and reproduced here.
    let mut out48 = [0u8; 48];
    kdf_sha256(&[0x0b; 20], b"prefix", b"Hi There", &mut out48).unwrap();
    assert_eq!(
        encode(&out48),
        "225ee33d1611fa4d3c01300f58d96de0590afc10d827ca551ebbb730c189e9bc\
         24b980d32e6ce098b998f1764192cd7e"
    );
    let mut out32 = [0u8; 32];
    kdf_sha256(&[0x0b; 20], b"prefix", b"Hi There", &mut out32).unwrap();
    assert_eq!(
        encode(&out32),
        "d9a682ffca79a74a2c845500b9628c0470c0fb7ad5b418e62701457509f92887"
    );
    // L is part of the input, so the 32-byte output is not a prefix of the
    // 48-byte one (unlike the SHA-1 PRF).
    assert_ne!(out48[..32], out32);
}

#[test]
fn kdf_sha256_refuses_bad_output_sizes() {
    assert_eq!(kdf_sha256(b"k", b"l", b"c", &mut []), Err(Error::BadLength));
    let mut too_long = alloc::vec![0u8; KDF_MAX + 1];
    assert_eq!(
        kdf_sha256(b"k", b"l", b"c", &mut too_long),
        Err(Error::BadLength)
    );
    let mut max = alloc::vec![0u8; KDF_MAX];
    assert!(kdf_sha256(b"k", b"l", b"c", &mut max).is_ok());
}

// --- MICs -------------------------------------------------------------------

#[test]
fn hmac_sha1_128_rfc2202_case_2() {
    // RFC 2202 test case 2: key "Jefe", data "what do ya want for nothing?";
    // full tag effcdf6a...9a7c79, truncated to 128 bits.
    let mic = hmac_sha1_128(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(encode(&mic), "effcdf6ae5eb2fa2d27416d5f184df9c");
}

#[test]
fn hmac_sha1_128_rfc2202_case_1_and_long_key() {
    // RFC 2202 case 1: key 0x0b * 20, "Hi There" -> b617318655057264e28bc0b6fb378c8e f146be00.
    let mic = hmac_sha1_128(&[0x0b; 20], b"Hi There");
    assert_eq!(encode(&mic), "b617318655057264e28bc0b6fb378c8e");
    // RFC 2202 case 6: an 80-byte key (longer than the block), prefix of
    // aa4ae5e15272d00e95705637ce8a3b55ed402112.
    let mic = hmac_sha1_128(
        &[0xaa; 80],
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(encode(&mic), "aa4ae5e15272d00e95705637ce8a3b55");
}

const CMAC_KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
const CMAC_MSG: &str = "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51\
                        30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710";

#[test]
fn aes_cmac_rfc4493_examples() {
    // RFC 4493 section 4, examples 1-4 (lengths 0, 16, 40, 64).
    let key: [u8; 16] = h(CMAC_KEY).try_into().unwrap();
    let msg = h(CMAC_MSG);
    let cases = [
        (0, "bb1d6929e95937287fa37d129b756746"),
        (16, "070a16b46b4d4144f79bdd9dd04a287c"),
        (40, "dfa66747de9ae63030ca32611497c827"),
        (64, "51f0bebf7e3b9d92fc49741779363cfe"),
    ];
    for (len, expected) in cases {
        assert_eq!(
            encode(&aes_cmac_128(&key, &msg[..len])),
            expected,
            "length {len}"
        );
    }
}

#[test]
fn mic_compare_checks_length_and_content() {
    let mic = hmac_sha1_128(b"k", b"m");
    assert!(mic_eq(&mic, &mic));
    let mut other = mic;
    other[15] ^= 1;
    assert!(!mic_eq(&mic, &other));
    assert!(!mic_eq(&mic, &mic[..15]));
    assert!(!mic_eq(&[], &mic));
}

// --- AES key wrap -----------------------------------------------------------

#[test]
fn key_wrap_rfc3394_section_4_1() {
    // RFC 3394 section 4.1: 128 bits of key data wrapped with a 128-bit KEK.
    let kek = h("000102030405060708090A0B0C0D0E0F");
    let plain = h("00112233445566778899AABBCCDDEEFF");
    let wrapped = aes_wrap(&kek, &plain).unwrap();
    assert_eq!(
        encode(&wrapped),
        "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5"
    );
    assert_eq!(aes_unwrap(&kek, &wrapped).unwrap(), plain);
}

#[test]
fn key_wrap_rfc3394_section_4_6() {
    // RFC 3394 section 4.6: 256 bits of key data wrapped with a 256-bit KEK.
    let kek = h("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F");
    let plain = h("00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F");
    let wrapped = aes_wrap(&kek, &plain).unwrap();
    assert_eq!(
        encode(&wrapped),
        "28c9f404c4b810f4cbccb35cfb87f8263f5786e2d80ed326cbc7f0e71a99f43bfb988b9b7a02dd21"
    );
    assert_eq!(aes_unwrap(&kek, &wrapped).unwrap(), plain);
}

#[test]
fn key_unwrap_fails_on_integrity_error() {
    let kek = h("000102030405060708090A0B0C0D0E0F");
    let wrapped = h("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    // Every single-bit flip must be refused.
    for byte in 0..wrapped.len() {
        for bit in 0..8 {
            let mut bad = wrapped.clone();
            bad[byte] ^= 1 << bit;
            assert_eq!(
                aes_unwrap(&kek, &bad),
                Err(Error::BadTag),
                "byte {byte} bit {bit}"
            );
        }
    }
    // A wrong KEK is the same answer.
    let wrong = h("0f0e0d0c0b0a09080706050403020100");
    assert_eq!(aes_unwrap(&wrong, &wrapped), Err(Error::BadTag));
}

#[test]
fn key_wrap_refuses_bad_lengths() {
    let kek = [7u8; 16];
    // Wrap: payload must be a multiple of 8 and at least 16.
    for len in [0, 8, 15, 17, 23] {
        assert_eq!(
            aes_wrap(&kek, &alloc::vec![0u8; len]),
            Err(Error::BadLength),
            "{len}"
        );
    }
    // Unwrap: at least 24, a multiple of 8, however hostile the length.
    for len in [0, 1, 8, 16, 23, 25, 31] {
        assert_eq!(
            aes_unwrap(&kek, &alloc::vec![0u8; len]),
            Err(Error::BadLength),
            "{len}"
        );
    }
    // KEK sizes: 16 and 32 only.
    for len in [0, 15, 17, 24, 31, 33, 64] {
        let bad = alloc::vec![1u8; len];
        assert_eq!(
            aes_wrap(&bad, &[0u8; 16]),
            Err(Error::BadLength),
            "kek {len}"
        );
        assert_eq!(
            aes_unwrap(&bad, &[0u8; 24]),
            Err(Error::BadLength),
            "kek {len}"
        );
    }
    // A large, well-formed all-zero blob is a clean integrity failure.
    assert_eq!(
        aes_unwrap(&kek, &alloc::vec![0u8; 4096]),
        Err(Error::BadTag)
    );
}
