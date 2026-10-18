//! CRC-32 (IEEE 802.3, reflected) of a frame: the headless mode prints one so
//! a timedemo run can be compared across boots without shipping pixels.

/// The CRC-32 of `bytes`, continuing from `crc` (start with 0).
pub fn update(crc: u32, bytes: &[u8]) -> u32 {
    let mut value = !crc;
    for &byte in bytes {
        value ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (value & 1).wrapping_neg();
            value = (value >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !value
}

/// The CRC-32 of a frame of XRGB pixels, in little-endian byte order (the
/// engine's own memory layout), so the value matches a dump of the buffer.
pub fn frame(pixels: &[u32]) -> u32 {
    pixels
        .iter()
        .fold(0, |crc, pixel| update(crc, &pixel.to_le_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_standard_check_value() {
        assert_eq!(update(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(update(0, b""), 0);
    }

    #[test]
    fn chunks_compose() {
        let whole = update(0, b"hello world");
        assert_eq!(update(update(0, b"hello "), b"world"), whole);
    }

    #[test]
    fn a_frame_is_its_little_endian_bytes() {
        let pixels = [0x0011_2233u32, 0xAABB_CCDD];
        let bytes = [0x33, 0x22, 0x11, 0x00, 0xDD, 0xCC, 0xBB, 0xAA];
        assert_eq!(frame(&pixels), update(0, &bytes));
    }
}
