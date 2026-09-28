//! SHA-256 (FIPS 180-4), provided by the vetted RustCrypto `sha2` crate.
//!
//! LazyOS does not hand-roll primitives (`docs/security-model.md` section 8):
//! this module is a thin, `no_std` façade over `sha2` so every caller uses one
//! import path and the backend choice (`force-soft`) lives in one place. The
//! host tests below pin the crate to the published FIPS 180-4 / RFC 6234
//! known-answer vectors, so a dependency bump that changes behaviour fails
//! `cargo test -p lazyos-crypto` before it can reach `keyd`.

/// Re-export the streaming digest API and the concrete SHA-256 type, so
/// callers write `use lazyos_crypto::sha256::{Digest, Sha256}`.
pub use sha2::{Digest, Sha256};

/// Digest length in bytes.
pub const DIGEST_LEN: usize = 32;

/// One-shot SHA-256 over `data`.
pub fn sha256(data: &[u8]) -> [u8; DIGEST_LEN] {
    let digest = Sha256::digest(data);
    let mut out = [0u8; DIGEST_LEN];
    out.copy_from_slice(&digest);
    out
}

/// SHA-256 over a sequence of parts, without allocating a concatenated copy.
pub fn sha256_parts(parts: &[&[u8]]) -> [u8; DIGEST_LEN] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    let mut out = [0u8; DIGEST_LEN];
    out.copy_from_slice(&digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 / RFC 6234 appendix B vectors plus the classic `"abc"`.
    #[test]
    fn known_answer_vectors() {
        let vectors: &[(&[u8], [u8; 32])] = &[
            (
                b"",
                [
                    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99,
                    0x6f, 0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95,
                    0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
                ],
            ),
            (
                b"abc",
                [
                    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d,
                    0xae, 0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10,
                    0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
                ],
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                [
                    0x24, 0x8d, 0x6a, 0x61, 0xd2, 0x06, 0x38, 0xb8, 0xe5, 0xc0, 0x26, 0x93, 0x0c,
                    0x3e, 0x60, 0x39, 0xa3, 0x3c, 0xe4, 0x59, 0x64, 0xff, 0x21, 0x67, 0xf6, 0xec,
                    0xed, 0xd4, 0x19, 0xdb, 0x06, 0xc1,
                ],
            ),
        ];
        for (message, expected) in vectors {
            assert_eq!(&sha256(message), expected, "message {message:?}");
        }
    }

    /// One million `'a'` bytes (FIPS 180-4 long-message vector); exercises the
    /// block-buffer path rather than a single padded block.
    #[test]
    fn million_a_vector() {
        let block = [b'a'; 1000];
        let mut hasher = Sha256::new();
        for _ in 0..1000 {
            hasher.update(block);
        }
        let digest = hasher.finalize();
        let expected = [
            0xcd, 0xc7, 0x6e, 0x5c, 0x99, 0x14, 0xfb, 0x92, 0x81, 0xa1, 0xc7, 0xe2, 0x84, 0xd7,
            0x3e, 0x67, 0xf1, 0x80, 0x9a, 0x48, 0xa4, 0x97, 0x20, 0x0e, 0x04, 0x6d, 0x39, 0xcc,
            0xc7, 0x11, 0x2c, 0xd0,
        ];
        assert_eq!(digest.as_slice(), expected);
    }

    /// The multi-part helper must equal the one-shot hash of the same bytes.
    #[test]
    fn parts_match_oneshot() {
        let parts: &[&[u8]] = &[b"lazy", b"OS", b" #102"];
        assert_eq!(sha256_parts(parts), sha256(b"lazyOS #102"));
    }
}
