//! HMAC-SHA256 (RFC 2104), provided by the vetted RustCrypto `hmac` crate.
//!
//! `keyd` uses HMAC as its MAC for `Sign` and as the tag half of `wrap`.
//! Verification goes through [`hmac_sha256_verify`], which delegates to the
//! crate's constant-time comparison; a manual `==` on tags would leak timing.

/// Re-export the MAC traits and the HMAC wrapper.
pub use hmac::{Hmac, Mac};

use crate::sha256::Sha256;

/// HMAC-SHA256 with a 32-byte tag.
pub type HmacSha256 = Hmac<Sha256>;

/// Tag length in bytes.
pub const TAG_LEN: usize = 32;

/// Compute `HMAC-SHA256(key, parts)` into `out`, streaming the parts.
///
/// Any key length is legal (RFC 2104 hashes keys longer than the 64-byte block
/// size); callers that need a minimum (e.g. "at least 128 bits of entropy")
/// enforce it themselves.
pub fn hmac_sha256_parts(key: &[u8], parts: &[&[u8]], out: &mut [u8; TAG_LEN]) {
    // `new_from_slice` accepts any length; the HMAC keys here are fixed-size
    // digests, so the `InvalidLength` arm is unreachable.
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(part);
    }
    out.copy_from_slice(&mac.finalize().into_bytes());
}

/// One-shot HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; TAG_LEN] {
    let mut out = [0u8; TAG_LEN];
    hmac_sha256_parts(key, &[data], &mut out);
    out
}

/// Constant-time tag check. `true` only when `tag` is exactly the HMAC of
/// `parts` under `key`.
pub fn hmac_sha256_verify(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> bool {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in parts {
        mac.update(part);
    }
    // `verify_slice` compares in constant time for equal lengths and rejects
    // mismatched lengths before touching the tag.
    mac.verify_slice(tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> alloc::string::String {
        use core::fmt::Write;
        let mut text = alloc::string::String::new();
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }

    /// RFC 4231 section 4 test cases (the HMAC-SHA256 half of them).
    #[test]
    fn rfc4231_known_answer_vectors() {
        let long_key = [0xaau8; 131];
        let long_data = b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.";
        let cases: &[(&[u8], &[u8], &str)] = &[
            (
                &[0x0bu8; 20],
                b"Hi There",
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                b"Jefe",
                b"what do ya want for nothing?",
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                &[0xaau8; 20],
                &[0xddu8; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            (
                &[0xaau8; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
            (
                &long_key,
                long_data,
                "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
            ),
        ];
        for (key, data, expected) in cases {
            let tag = hmac_sha256(key, data);
            assert_eq!(hex(&tag), *expected, "key {key:?} data {data:?}");
            assert!(hmac_sha256_verify(key, &[data], &tag));
        }
    }

    /// The multi-part helper streams the same bytes as one update, and a
    /// corrupted tag or key is rejected.
    #[test]
    fn parts_and_tamper_detection() {
        let key = b"kdf-master-key-material";
        let parts: &[&[u8]] = &[b"lazy", b"OS", b"-102"];
        let tag = hmac_sha256(key, b"lazyOS-102");
        assert!(hmac_sha256_verify(key, parts, &tag));

        let mut bad = tag;
        bad[0] ^= 1;
        assert!(!hmac_sha256_verify(key, parts, &bad));
        assert!(!hmac_sha256_verify(b"other-key", parts, &tag));
        assert!(!hmac_sha256_verify(key, parts, &tag[..31]));
    }
}
