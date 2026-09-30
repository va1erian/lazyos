#![forbid(unsafe_code)]

//! Parsing the canvas size a user types into the Resize prompt.

use super::{MAX_SIDE, MIN_SIDE};

const USAGE: &str = "enter a size as WIDTHxHEIGHT";

/// Parses `"640x480"`, `"640 x 480"`, `"640,480"` (also `X`, `*` and the
/// multiplication sign) into `(width, height)`, each in `MIN_SIDE..=MAX_SIDE`.
/// The error is a short message for the status bar.
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let mut parts = text.split(['x', 'X', '*', ',', '\u{d7}']);
    let (Some(width), Some(height), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(USAGE.to_string());
    };
    Ok((side(width)?, side(height)?))
}

fn side(part: &str) -> Result<u32, String> {
    let part = part.trim();
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(USAGE.to_string());
    }
    // A value too long for u32 is far out of range anyway.
    let value: u32 = part.parse().unwrap_or(u32::MAX);
    if (MIN_SIDE..=MAX_SIDE).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "size must be {MIN_SIDE}-{MAX_SIDE} pixels per side"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_supported_separators() {
        for text in [
            "640x480",
            "640 x 480",
            " 640,480 ",
            "640X480",
            "640*480",
            "640\u{d7}480",
        ] {
            assert_eq!(parse_size(text), Ok((640, 480)), "{text:?}");
        }
    }

    #[test]
    fn accepts_the_limits() {
        assert_eq!(parse_size("1x1"), Ok((1, 1)));
        assert_eq!(parse_size("1024x1024"), Ok((1024, 1024)));
    }

    #[test]
    fn rejects_malformed_and_out_of_range_input() {
        for text in [
            "",
            "640",
            "x",
            "640x",
            "x480",
            "0x10",
            "10x0",
            "1025x10",
            "10x1025",
            "-5x10",
            "+5x10",
            "1x2x3",
            "axb",
            "6 4x8",
            "99999999999x5",
            "1.5x2",
        ] {
            assert!(parse_size(text).is_err(), "{text:?} should be rejected");
        }
    }
}
