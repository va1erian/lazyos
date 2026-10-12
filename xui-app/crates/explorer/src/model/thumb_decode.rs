#![forbid(unsafe_code)]

//! Turning a picture file's bytes into a small thumbnail.
//!
//! This runs on a worker thread, never the UI thread. It reads the image's
//! dimensions from the header first, so a small file that declares a huge
//! canvas (a decompression bomb) is refused before anything is allocated, then
//! decodes through `xui-core` (PNG and JPEG) and shrinks with a box filter,
//! which keeps a photo's detail where the bilinear `Image::resized` would
//! alias.

use xui_core::image::Image;

/// The longest side of a thumbnail, in pixels: a 48 dip tile at 2x.
pub(crate) const EDGE: u32 = 128;

/// The most pixels a picture may declare. A decoded picture is four bytes a
/// pixel and two workers decode at once, so this bounds the transient memory
/// to about 100 MB on the 1 GiB guest.
pub(crate) const MAX_PIXELS: u64 = 12_000_000;

/// Whether a file name is one the decoder handles (case-insensitive).
pub(crate) fn supported(name: &std::ffi::OsStr) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg"
            )
        })
}

/// The thumbnail of the picture in `bytes`, or `None` when it is not a
/// supported picture, is too large, or does not decode.
pub(crate) fn thumbnail(bytes: &[u8]) -> Option<Image> {
    let (width, height) = dimensions(bytes)?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    shrink(Image::decode(bytes).ok()?)
}

/// The width and height a PNG or JPEG header declares.
pub(crate) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.starts_with(&PNG) {
        // The IHDR chunk comes first: 8 signature + 4 length + 4 type.
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return Some((width, height));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_dimensions(bytes);
    }
    None
}

/// Walks the JPEG's marker segments to the first start-of-frame.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| -> Option<usize> {
        Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize)
    };
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        // Any number of 0xFF fill bytes may precede a marker.
        while *bytes.get(at + 1)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            // SOF0..SOF15, except DHT (C4), JPG (C8) and DAC (CC).
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                return Some((be16(at + 7)? as u32, be16(at + 5)? as u32));
            }
            // Standalone markers have no length.
            0x01 | 0xD0..=0xD8 => at += 2,
            // Start of scan or end of image before a frame header: none.
            0xD9 | 0xDA => return None,
            _ => at += 2 + be16(at + 2)?,
        }
    }
}

/// Scales `image` so its longest side is at most [`EDGE`], averaging every
/// source pixel into the destination pixel it falls in. A picture already that
/// small is returned as it is.
fn shrink(image: Image) -> Option<Image> {
    let (width, height) = image.size();
    let longest = width.max(height);
    if longest <= EDGE {
        return Some(image);
    }
    let scale =
        |side: u32| ((u64::from(side) * u64::from(EDGE)) / u64::from(longest)).max(1) as u32;
    let (out_width, out_height) = (scale(width), scale(height));
    let source = image.pixels();
    let mut out = vec![0u8; out_width as usize * out_height as usize * 4];
    for dy in 0..out_height {
        let y0 = (u64::from(dy) * u64::from(height) / u64::from(out_height)) as u32;
        let y1 = ((u64::from(dy + 1) * u64::from(height) / u64::from(out_height)) as u32)
            .max(y0 + 1)
            .min(height);
        for dx in 0..out_width {
            let x0 = (u64::from(dx) * u64::from(width) / u64::from(out_width)) as u32;
            let x1 = ((u64::from(dx + 1) * u64::from(width) / u64::from(out_width)) as u32)
                .max(x0 + 1)
                .min(width);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = (y as usize * width as usize + x0 as usize) * 4;
                for pixel in source[row..row + (x1 - x0) as usize * 4].chunks(4) {
                    for (total, value) in sum.iter_mut().zip(pixel) {
                        *total += u32::from(*value);
                    }
                }
            }
            let count = (x1 - x0) * (y1 - y0);
            let at = (dy as usize * out_width as usize + dx as usize) * 4;
            for (slot, total) in out[at..at + 4].iter_mut().zip(sum) {
                *slot = (total / count) as u8;
            }
        }
    }
    Image::from_rgba(out_width, out_height, out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let pixels = rgba.repeat(width as usize * height as usize);
        Image::from_rgba(width, height, pixels)
            .unwrap()
            .encode_png()
            .unwrap()
    }

    #[test]
    fn a_large_picture_shrinks_to_the_edge_keeping_its_aspect() {
        let thumb = thumbnail(&png(1000, 500, [200, 40, 10, 255])).unwrap();
        assert_eq!(thumb.size(), (EDGE, EDGE / 2));
        assert_eq!(thumb.pixel(5, 5), Some([200, 40, 10, 255]));
    }

    #[test]
    fn a_small_picture_is_kept_as_it_is() {
        let thumb = thumbnail(&png(20, 30, [1, 2, 3, 255])).unwrap();
        assert_eq!(thumb.size(), (20, 30));
    }

    #[test]
    fn the_box_filter_averages_a_checkerboard() {
        let (w, h) = (256u32, 256u32);
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0 } else { 254 };
                pixels.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let thumb = shrink(Image::from_rgba(w, h, pixels).unwrap()).unwrap();
        let [r, ..] = thumb.pixel(10, 10).unwrap();
        assert!((120..=134).contains(&r), "mid grey, got {r}");
    }

    #[test]
    fn headers_that_declare_too_much_are_refused_before_decoding() {
        let mut bytes = png(4, 4, [0, 0, 0, 255]);
        bytes[16..20].copy_from_slice(&40_000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&40_000u32.to_be_bytes());
        assert_eq!(dimensions(&bytes), Some((40_000, 40_000)));
        assert!(thumbnail(&bytes).is_none());
    }

    #[test]
    fn garbage_and_truncations_are_none() {
        assert!(thumbnail(b"").is_none());
        assert!(thumbnail(b"GIF89a....").is_none());
        let bytes = png(300, 300, [9, 9, 9, 255]);
        assert!(thumbnail(&bytes[..bytes.len() / 2]).is_none());
        assert!(dimensions(&[0xFF, 0xD8, 0xFF]).is_none());
    }

    #[test]
    fn jpeg_dimensions_come_from_the_frame_header() {
        // SOI, APP0 (length 4), SOF0 with 480 high and 640 wide.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x01,
            0xE0, 0x02, 0x80, 0x01, 0x01, 0x11, 0x00,
        ];
        assert_eq!(dimensions(&jpeg), Some((640, 480)));
    }

    #[test]
    fn only_png_and_jpeg_names_are_supported() {
        use std::ffi::OsStr;
        assert!(supported(OsStr::new("a.PNG")));
        assert!(supported(OsStr::new("a.jpeg")));
        assert!(!supported(OsStr::new("a.gif")));
        assert!(!supported(OsStr::new("png")));
    }
}
