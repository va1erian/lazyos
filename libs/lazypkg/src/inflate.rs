//! Entry decompression and CRC-32.
//!
//! Only stored (method 0) and raw DEFLATE (method 8) are accepted. DEFLATE is
//! inflated with an output cap equal to the declared uncompressed size, so a
//! stream that tries to expand past it fails cheaply; the caller then checks
//! the byte count and CRC-32 against the central directory.

use alloc::vec::Vec;
use crc::{Crc, CRC_32_ISO_HDLC};

/// Why inflating an entry failed before the CRC check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InflateError {
    /// The compressed bytes are not a valid stream of the given method.
    Corrupt,
    /// The stream produced a different number of bytes than declared.
    SizeMismatch { expected: u32, actual: u32 },
}

/// CRC-32 (IEEE 802.3) of `data`.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    Crc::<u32>::new(&CRC_32_ISO_HDLC).checksum(data)
}

/// Inflate (or copy, for stored) one entry into a new `Vec`, bounded by `size`.
pub(crate) fn decompress(method: u16, data: &[u8], size: u32) -> Result<Vec<u8>, InflateError> {
    let expected = size as usize;
    match method {
        0 => {
            if data.len() != expected {
                return Err(InflateError::SizeMismatch {
                    expected: size,
                    actual: data.len().min(u32::MAX as usize) as u32,
                });
            }
            Ok(data.to_vec())
        }
        8 => {
            let out = miniz_oxide::inflate::decompress_to_vec_with_limit(data, expected)
                .map_err(|_| InflateError::Corrupt)?;
            if out.len() != expected {
                return Err(InflateError::SizeMismatch {
                    expected: size,
                    actual: out.len().min(u32::MAX as usize) as u32,
                });
            }
            Ok(out)
        }
        _ => Err(InflateError::Corrupt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_vector() {
        // The classic IEEE 802.3 check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn stored_copy_is_exact() {
        assert_eq!(decompress(0, b"hello", 5), Ok(b"hello".to_vec()));
        assert_eq!(
            decompress(0, b"hello", 4),
            Err(InflateError::SizeMismatch {
                expected: 4,
                actual: 5
            })
        );
    }

    #[test]
    fn deflate_round_trips_and_respects_the_cap() {
        let plain = b"lazyos package test data ".repeat(64);
        let compressed = miniz_oxide::deflate::compress_to_vec(&plain, 6);
        assert_eq!(
            decompress(8, &compressed, plain.len() as u32),
            Ok(plain.clone())
        );
        // A smaller cap must fail rather than truncate.
        assert!(decompress(8, &compressed, plain.len() as u32 - 1).is_err());
        // Garbage must fail, not panic.
        assert_eq!(decompress(8, &[0xff; 32], 16), Err(InflateError::Corrupt));
    }

    #[test]
    fn unknown_methods_are_corrupt() {
        assert_eq!(decompress(99, b"x", 1), Err(InflateError::Corrupt));
    }
}
