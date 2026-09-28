//! Minimal lowercase hex encoding for diagnostics.
//!
//! `keyd` prints only public fingerprints on serial; the KATs compare hex
//! strings so a failure detail is readable without a second decode step. This
//! is *not* secret-bearing data.

use alloc::string::String;
use core::fmt::Write;

/// Lowercase hex for `bytes`, allocating the result.
pub fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing into a `String` never fails.
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_lowercase() {
        assert_eq!(encode(&[]), "");
        assert_eq!(encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
