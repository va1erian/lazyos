//! CRC-32 (IEEE 802.3, reflected) of a frame: the headless mode prints one so
//! a run can be compared across boots without shipping pixels.

const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut value = n as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[n] = value;
        n += 1;
    }
    table
};

/// The CRC-32 of `bytes`, continuing from `crc` (start with 0).
pub fn update(crc: u32, bytes: &[u8]) -> u32 {
    let mut value = !crc;
    for &byte in bytes {
        value = (value >> 8) ^ TABLE[((value ^ u32::from(byte)) & 0xFF) as usize];
    }
    !value
}

/// The CRC-32 of an 8-bit frame as the player sees it: each pixel expanded
/// through `palette` (`r, g, b` per index) and hashed as those three bytes, rows
/// top to bottom, the row padding (`pitch - width`) left out. Returns `None`
/// when the buffer is smaller than `pitch * height` or `pitch < width`.
pub fn frame(
    pixels: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
    palette: &[u8; 768],
) -> Option<u32> {
    if pitch < width || pixels.len() < pitch.checked_mul(height)? {
        return None;
    }
    let mut crc = 0;
    let mut row_rgb = Vec::with_capacity(width * 3);
    for row in pixels.chunks(pitch).take(height) {
        row_rgb.clear();
        for &index in &row[..width] {
            let at = usize::from(index) * 3;
            row_rgb.extend_from_slice(&palette[at..at + 3]);
        }
        crc = update(crc, &row_rgb);
    }
    Some(crc)
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
    fn a_frame_is_its_palette_expanded_rows_without_padding() {
        let mut palette = [0u8; 768];
        palette[3..6].copy_from_slice(&[1, 2, 3]);
        palette[6..9].copy_from_slice(&[4, 5, 6]);
        // 2x2, pitch 3: the third column of each row is padding
        let pixels = [1u8, 2, 255, 2, 1, 255];
        let expected = update(0, &[1, 2, 3, 4, 5, 6, 4, 5, 6, 1, 2, 3]);
        assert_eq!(frame(&pixels, 2, 2, 3, &palette), Some(expected));
    }

    #[test]
    fn a_short_buffer_or_pitch_is_refused() {
        let palette = [0u8; 768];
        assert_eq!(frame(&[0; 5], 2, 2, 3, &palette), None);
        assert_eq!(frame(&[0; 8], 4, 2, 2, &palette), None);
        assert_eq!(frame(&[], usize::MAX, 2, usize::MAX, &palette), None);
    }
}
