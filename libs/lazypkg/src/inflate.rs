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
#[cfg(test)]
pub(crate) fn decompress(method: u16, data: &[u8], size: u32) -> Result<Vec<u8>, InflateError> {
    let mut out = Vec::new();
    decompress_into(method, data, size, &mut out)?;
    Ok(out)
}

/// Inflate (or copy, for stored) one entry into `out`, replacing its contents,
/// bounded by `size`.
///
/// `out` is sized once to the declared size and filled in place, so a caller
/// that passes the same `Vec` for every entry reuses one allocation. That is
/// what keeps a long-lived reader bounded: the user heap never reuses a block
/// over 1 MiB, and growing a fresh `Vec` while inflating would leave every
/// intermediate size behind. Large entries are better streamed
/// ([`crate::chunks`]), which never holds a whole entry.
pub(crate) fn decompress_into(
    method: u16,
    data: &[u8],
    size: u32,
    out: &mut Vec<u8>,
) -> Result<(), InflateError> {
    let expected = size as usize;
    out.clear();
    match method {
        0 => {
            if data.len() != expected {
                return Err(InflateError::SizeMismatch {
                    expected: size,
                    actual: data.len().min(u32::MAX as usize) as u32,
                });
            }
            out.extend_from_slice(data);
            Ok(())
        }
        8 => {
            use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
            use miniz_oxide::inflate::TINFLStatus;
            out.resize(expected, 0);
            // About 11 KiB: boxed so it never lands on a small stack, and small
            // enough for the user heap to recycle.
            let mut state = alloc::boxed::Box::<DecompressorOxide>::default();
            let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
            let (status, _read, written) = decompress(&mut state, data, out, 0, flags);
            match status {
                TINFLStatus::Done if written == expected => Ok(()),
                TINFLStatus::Done => {
                    out.truncate(written);
                    Err(InflateError::SizeMismatch {
                        expected: size,
                        actual: written.min(u32::MAX as usize) as u32,
                    })
                }
                // The stream wants to write past the declared size.
                TINFLStatus::HasMoreOutput => Err(InflateError::SizeMismatch {
                    expected: size,
                    actual: u32::MAX,
                }),
                _ => Err(InflateError::Corrupt),
            }
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

    #[test]
    fn decompress_into_reuses_the_buffer() {
        let plain = b"reuse me ".repeat(4096);
        let compressed = miniz_oxide::deflate::compress_to_vec(&plain, 6);
        let mut out = Vec::with_capacity(plain.len());
        let before = out.as_ptr();
        decompress_into(8, &compressed, plain.len() as u32, &mut out).unwrap();
        assert_eq!(out, plain);
        assert_eq!(out.as_ptr(), before, "inflating moved the buffer");
        decompress_into(0, b"short", 5, &mut out).unwrap();
        assert_eq!(out, b"short");
        assert_eq!(out.as_ptr(), before, "a stored copy moved the buffer");
        // A declared size the stream does not fill is a mismatch.
        assert!(decompress_into(8, &compressed, plain.len() as u32 + 1, &mut out).is_err());
        assert!(out.len() <= plain.len());
    }
}
