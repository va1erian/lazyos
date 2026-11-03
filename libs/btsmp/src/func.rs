//! The SMP cryptographic toolbox (Core Specification Vol 3, Part H, 2.2.6 to
//! 2.2.9 and 2.2.2): `f4`, `f5`, `f6`, `g2` and `ah`.
//!
//! **Byte order.** The specification writes every value most-significant
//! octet first, and so do its sample data (Appendix D). On the air every
//! multi-octet value is little-endian. These functions take and return the
//! *specification* form so the published vectors can be pasted into tests
//! unchanged; [`crate::pairing`] reverses at the boundary and nowhere else.

use lazyos_crypto::aes::{aes128_encrypt_block, aes_cmac_128};

/// `SALT` of `f5` (2.2.7), most-significant octet first.
const F5_SALT: [u8; 16] = [
    0x6C, 0x88, 0x83, 0x91, 0xAA, 0xF5, 0xA5, 0x38, 0x60, 0x37, 0x0B, 0xDB, 0x5A, 0x60, 0x83, 0xBE,
];
/// `keyID` of `f5`: the ASCII string "btle".
const F5_KEY_ID: [u8; 4] = *b"btle";

/// A device address as `f5`/`f6` take it: the address-type octet then the six
/// address octets, most significant first (56 bits).
pub type Addr56 = [u8; 7];

/// `f4(U, V, X, Z) = AES-CMAC_X(U || V || Z)`: the LE SC confirm value.
/// `u` and `v` are public-key X coordinates, `x` a nonce.
pub fn f4(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], z: u8) -> [u8; 16] {
    let mut msg = [0u8; 65];
    msg[..32].copy_from_slice(u);
    msg[32..64].copy_from_slice(v);
    msg[64] = z;
    aes_cmac_128(x, &msg)
}

/// `f5(W, N1, N2, A1, A2)`: the MacKey and the LTK from the DH key.
/// Returns `(mac_key, ltk)`.
pub fn f5(
    w: &[u8; 32],
    n1: &[u8; 16],
    n2: &[u8; 16],
    a1: &Addr56,
    a2: &Addr56,
) -> ([u8; 16], [u8; 16]) {
    let t = aes_cmac_128(&F5_SALT, w);
    let block = |counter: u8| {
        let mut msg = [0u8; 53];
        msg[0] = counter;
        msg[1..5].copy_from_slice(&F5_KEY_ID);
        msg[5..21].copy_from_slice(n1);
        msg[21..37].copy_from_slice(n2);
        msg[37..44].copy_from_slice(a1);
        msg[44..51].copy_from_slice(a2);
        // Length: the 256 bits of output (MacKey || LTK), as a 16-bit number.
        msg[51..53].copy_from_slice(&256u16.to_be_bytes());
        aes_cmac_128(&t, &msg)
    };
    (block(0), block(1))
}

/// `f6(W, N1, N2, R, IOcap, A1, A2)`: the DHKey check value. `w` is the
/// MacKey; `io_cap` is `AuthReq || OOB flag || IO capability` of the device the
/// check belongs to.
pub fn f6(
    w: &[u8; 16],
    n1: &[u8; 16],
    n2: &[u8; 16],
    r: &[u8; 16],
    io_cap: &[u8; 3],
    a1: &Addr56,
    a2: &Addr56,
) -> [u8; 16] {
    let mut msg = [0u8; 65];
    msg[..16].copy_from_slice(n1);
    msg[16..32].copy_from_slice(n2);
    msg[32..48].copy_from_slice(r);
    msg[48..51].copy_from_slice(io_cap);
    msg[51..58].copy_from_slice(a1);
    msg[58..65].copy_from_slice(a2);
    aes_cmac_128(w, &msg)
}

/// `g2(U, V, X, Y)`: the 32-bit value Numeric Comparison shows (the caller
/// takes it modulo 10^6).
pub fn g2(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], y: &[u8; 16]) -> u32 {
    let mut msg = [0u8; 80];
    msg[..32].copy_from_slice(u);
    msg[32..64].copy_from_slice(v);
    msg[64..].copy_from_slice(y);
    let mac = aes_cmac_128(x, &msg);
    u32::from_be_bytes([mac[12], mac[13], mac[14], mac[15]])
}

/// `ah(k, r)`: the 24-bit hash of a resolvable private address (2.2.2),
/// `e(k, padding || r)` truncated to its low 24 bits. `r` is `prand`, 24 bits,
/// most significant octet first.
pub fn ah(irk: &[u8; 16], r: &[u8; 3]) -> [u8; 3] {
    let mut block = [0u8; 16];
    block[13..].copy_from_slice(r);
    let out = aes128_encrypt_block(irk, &block);
    [out[13], out[14], out[15]]
}

/// Whether the 48-bit address `addr` (as HCI carries it, least significant
/// octet first) is a resolvable private address made from `irk`: its two top
/// bits are `01` and its hash part equals `ah(irk, prand)`.
pub fn rpa_matches(irk: &[u8; 16], addr: &[u8; 6]) -> bool {
    if addr[5] & 0xC0 != 0x40 {
        return false;
    }
    let prand = [addr[5], addr[4], addr[3]];
    let hash = [addr[2], addr[1], addr[0]];
    lazyos_crypto::wifi::mic_eq(&ah(irk, &prand), &hash)
}

/// A fresh resolvable private address for `irk` from 22 random bits `seed`
/// (the top two bits are forced to `01`), in HCI order. Used by tests and by
/// a future privacy-enabled host.
pub fn make_rpa(irk: &[u8; 16], seed: [u8; 3]) -> [u8; 6] {
    let prand = [(seed[0] & 0x3F) | 0x40, seed[1], seed[2]];
    let hash = ah(irk, &prand);
    [hash[2], hash[1], hash[0], prand[2], prand[1], prand[0]]
}
