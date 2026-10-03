#![forbid(unsafe_code)]

//! Reads a picture's pixel size from its header, before decoding it.
//!
//! A 16 MiB file can claim far more pixels than LazyOS can hold once decoded
//! (a "decompression bomb"), so Insert image checks the header first and
//! refuses an oversize picture without allocating for it.

/// The most pixels an inserted picture may have: 4096 x 4096, 64 MiB decoded.
pub const MAX_IMAGE_PIXELS: u64 = 4096 * 4096;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The `(width, height)` a PNG or JPEG declares, or `None` when the header is
/// not one of them or is cut short.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&PNG_SIGNATURE) {
        png_dimensions(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg_dimensions(bytes)
    } else {
        None
    }
}

/// Whether a picture of `size` is small enough to decode.
pub fn fits(size: (u32, u32)) -> bool {
    let (w, h) = size;
    w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    let b = bytes.get(at..at + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The IHDR chunk is first: length (4), `IHDR` (4), width (4), height (4).
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    Some((be32(bytes, 16)?, be32(bytes, 20)?))
}

/// Walks the JPEG markers to the first start-of-frame (SOF0 to SOF15, except
/// DHT, JPG and DAC, which share the range), whose payload is precision (1),
/// height (2), width (2).
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        // Skip to the marker byte; fill bytes (0xFF) may repeat.
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            // Markers without a length.
            0x01 | 0xD0..=0xD7 => continue,
            0xD9 | 0xDA => return None,
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let height = be16(bytes, at + 3)?;
                let width = be16(bytes, at + 5)?;
                return Some((u32::from(width), u32::from(height)));
            }
            _ => {
                let length = usize::from(be16(bytes, at)?);
                if length < 2 {
                    return None;
                }
                at += length;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_core::Image;

    #[test]
    fn a_png_header_gives_its_size() {
        let image = Image::from_rgba(7, 3, vec![0; 7 * 3 * 4]).unwrap();
        let png = image.encode_png().unwrap();
        assert_eq!(dimensions(&png), Some((7, 3)));
    }

    #[test]
    fn a_jpeg_header_gives_its_size() {
        // SOI, an APP0 segment, then SOF0 for 640 x 480.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80]);
        assert_eq!(dimensions(&jpeg), Some((640, 480)));
    }

    #[test]
    fn garbage_and_truncated_headers_give_none() {
        assert_eq!(dimensions(b"hello"), None);
        assert_eq!(dimensions(&PNG_SIGNATURE), None);
        assert_eq!(dimensions(&[0xFF, 0xD8, 0xFF]), None);
        assert_eq!(dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01]), None);
        assert_eq!(dimensions(&[0xFF, 0xD8, 0x00]), None);
    }

    #[test]
    fn the_pixel_cap_refuses_a_bomb() {
        assert!(fits((4096, 4096)));
        assert!(fits((360, 200)));
        assert!(!fits((4097, 4096)));
        assert!(!fits((u32::MAX, u32::MAX)));
        assert!(!fits((0, 10)));
    }
}
