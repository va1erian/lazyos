//! Single-block AES-128 and AES-CMAC, the two primitives Bluetooth SMP is
//! built from (Core Specification Vol 3, Part H, 2.2: `e`, and `AES-CMAC`).
//!
//! `e` is raw ECB on one block; it exists for the resolvable-private-address
//! hash `ah`. Never use it on more than one block of anything.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;

pub use crate::wifi::aes_cmac_128;

/// AES-128 encryption of one 16-byte block (`e(key, plaintext)`).
pub fn aes128_encrypt_block(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(key.into());
    let mut out = (*block).into();
    cipher.encrypt_block(&mut out);
    out.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS-197 Appendix B.
    #[test]
    fn fips197_example() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let plain = [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d, 0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37,
            0x07, 0x34,
        ];
        let want = [
            0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb, 0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a,
            0x0b, 0x32,
        ];
        assert_eq!(aes128_encrypt_block(&key, &plain), want);
    }
}
