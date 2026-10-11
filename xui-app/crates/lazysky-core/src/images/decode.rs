//! Decoding with limits: a picture the server sends is untrusted input.
//!
//! The size is read from the header and refused *before* any pixel buffer is
//! allocated; the codec's own allocation limit backs that up.

use std::io::Cursor;

use image::{DynamicImage, ImageReader, Limits};

/// The longest side accepted.
pub const MAX_SIDE: u32 = 8192;
/// The most pixels accepted (a decode buffer is four bytes each).
pub const MAX_PIXELS: u64 = 64_000_000;

/// Decodes `bytes` (JPEG, PNG, GIF's first frame, WebP) if its dimensions
/// are within [`MAX_SIDE`] and [`MAX_PIXELS`].
pub fn decode_limited(bytes: &[u8]) -> Result<DynamicImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("not a picture: {e}"))?;
    let (width, height) = {
        let probe = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| format!("not a picture: {e}"))?;
        probe
            .into_dimensions()
            .map_err(|e| format!("not a picture this client reads: {e}"))?
    };
    if width == 0 || height == 0 {
        return Err("the picture is empty".into());
    }
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!(
            "the picture is {width}x{height}, larger than this client decodes"
        ));
    }
    let mut reader = reader;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_PIXELS * 4 + (16 << 20));
    reader.limits(limits);
    reader
        .decode()
        .map_err(|e| format!("the picture is damaged: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbaImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 30, 255]));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn a_small_png_decodes() {
        let img = decode_limited(&png(30, 20)).unwrap();
        assert_eq!((img.width(), img.height()), (30, 20));
    }

    #[test]
    fn garbage_is_not_a_picture() {
        assert!(decode_limited(b"<html>not an image</html>").is_err());
        assert!(decode_limited(b"").is_err());
        let mut cut = png(30, 20);
        cut.truncate(cut.len() / 2);
        assert!(decode_limited(&cut).unwrap_err().contains("damaged"));
    }

    /// A PNG whose header claims `width` x `height` and has no pixel data:
    /// what a decompression bomb's first bytes look like.
    fn claimed(width: u32, height: u32) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(&ihdr);
        out.extend_from_slice(&crc(&ihdr).to_be_bytes());
        // A first IDAT (an empty zlib stream) so the header is complete: a
        // reader that stops at the first data chunk has all it needs.
        let mut idat = b"IDAT".to_vec();
        idat.extend_from_slice(&[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&8u32.to_be_bytes());
        out.extend_from_slice(&idat);
        out.extend_from_slice(&crc(&idat).to_be_bytes());
        out
    }

    #[test]
    fn a_decode_bomb_is_refused_before_any_allocation() {
        for (w, h) in [
            (70_000, 70_000),
            (MAX_SIDE + 1, 10),
            (10, MAX_SIDE + 1),
            (8100, 8100),
        ] {
            let err = decode_limited(&claimed(w, h)).unwrap_err();
            assert!(
                err.contains("larger than this client decodes"),
                "{w}x{h}: {err}"
            );
        }
    }
}
