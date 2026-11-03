//! The Core Specification's sample data (Vol 3, Part H, Appendix D), typed in
//! as published: most-significant octet first, which is the form `func` takes.

use crate::func::*;

pub fn hex<const N: usize>(text: &str) -> [u8; N] {
    let digits: alloc::vec::Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert_eq!(digits.len(), 2 * N, "wrong hex length");
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = core::str::from_utf8(&digits[2 * i..2 * i + 2]).unwrap();
        *byte = u8::from_str_radix(pair, 16).unwrap();
    }
    out
}

const PUB_A_X: &str = "20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6";
const PUB_B_X: &str = "1ea1f0f0 1faf1d96 09592284 f19e4c00 47b58afd 8615a69f 559077b2 2faaa190";
const PRIV_A: &str = "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd";
const PRIV_B: &str = "55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd";
/// The V of the f4 and g2 samples (the same digits as private key B).
const SPEC_V: &str = "55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd";
const DH_KEY: &str = "ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698";

/// D.1: the sample public keys derive from the sample private keys.
#[test]
fn d1_ecdh_sample_keys() {
    use lazyos_crypto::p256::KeyPair;
    let a = KeyPair::from_secret_be(&hex(PRIV_A)).unwrap();
    let b = KeyPair::from_secret_be(&hex(PRIV_B)).unwrap();
    let mut ax: [u8; 32] = a.public_smp()[..32].try_into().unwrap();
    let mut bx: [u8; 32] = b.public_smp()[..32].try_into().unwrap();
    ax.reverse();
    bx.reverse();
    assert_eq!(ax, hex(PUB_A_X));
    assert_eq!(bx, hex(PUB_B_X));
}

/// D.2: f4 with U, V, X and Z = 0.
#[test]
fn d2_f4() {
    let got = f4(
        &hex(PUB_A_X),
        &hex(SPEC_V),
        &hex("d5cb8454 d177733e ffffb2ec 712baeab"),
        0,
    );
    assert_eq!(got, hex("f2c916f1 07a9bd1c f1eda1be a974872d"));
}

/// D.3: f5, with the intermediate T-derived MacKey and the LTK.
#[test]
fn d3_f5() {
    let (mac_key, ltk) = f5(
        &hex(DH_KEY),
        &hex("d5cb8454 d177733e ffffb2ec 712baeab"),
        &hex("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf"),
        &hex("00 56123737bfce"),
        &hex("00 a713702dcfc1"),
    );
    assert_eq!(mac_key, hex("2965f176 a1084a02 fd3f6a20 ce636e20"));
    assert_eq!(ltk, hex("69867911 69d7cd23 980522b5 94750a38"));
}

/// D.4: f6.
#[test]
fn d4_f6() {
    let got = f6(
        &hex("2965f176 a1084a02 fd3f6a20 ce636e20"),
        &hex("d5cb8454 d177733e ffffb2ec 712baeab"),
        &hex("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf"),
        &hex("12a3343b b453bb54 08da42d2 0c2d0fc8"),
        &hex("010102"),
        &hex("00 56123737bfce"),
        &hex("00 a713702dcfc1"),
    );
    assert_eq!(got, hex("e3c47398 9cd0e8c5 d26c0b09 da958f61"));
}

/// D.5: g2, and the six-digit number it gives.
#[test]
fn d5_g2() {
    let got = g2(
        &hex(PUB_A_X),
        &hex(SPEC_V),
        &hex("d5cb8454 d177733e ffffb2ec 712baeab"),
        &hex("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf"),
    );
    assert_eq!(got, 0x2f9e_d5ba);
    assert_eq!(got % 1_000_000, 938_554);
}

/// D.7: ah, and RPA resolution built on it.
#[test]
fn d7_ah_and_rpa() {
    let irk = hex("ec0234a3 57c8ad05 341010a6 0a397d9b");
    assert_eq!(ah(&irk, &hex("708194")), hex("0dfbaa"));
    // The address 70:81:94:0d:fb:aa in HCI order.
    let rpa = [0xaa, 0xfb, 0x0d, 0x94, 0x81, 0x70];
    assert!(rpa_matches(&irk, &rpa));
    let mut wrong = rpa;
    wrong[0] ^= 1;
    assert!(!rpa_matches(&irk, &wrong));
    // Not resolvable: the top two bits are not 01.
    let mut not_rpa = rpa;
    not_rpa[5] = 0xC0;
    assert!(!rpa_matches(&irk, &not_rpa));
    assert!(rpa_matches(&irk, &make_rpa(&irk, [0x12, 0x34, 0x56])));
}
