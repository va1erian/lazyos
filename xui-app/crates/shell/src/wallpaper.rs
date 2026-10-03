//! The desktop picture's arithmetic: how big a file claims to be before it is
//! decoded, which part of it covers the screen, and how bright it is where the
//! launcher labels sit.

use crate::Rect;

/// The PNG signature followed by the first chunk's length (13) and type.
const PNG_HEAD: [u8; 16] = [
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
];

/// The `(width, height)` a PNG or JPEG header declares, without decoding
/// anything; `None` for any other or a truncated file. The picture's path
/// comes from a setting, so its size is checked before a decoder allocates
/// the pixels the header asks for.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&PNG_HEAD) {
        let side = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        return Some((side(16)?, side(20)?));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_dimensions(&bytes[2..]);
    }
    None
}

/// Walk a JPEG's segments (after the SOI marker) to its frame header.
fn jpeg_dimensions(mut rest: &[u8]) -> Option<(u32, u32)> {
    loop {
        let (&[0xFF, marker], after) = rest.split_first_chunk::<2>()? else {
            return None;
        };
        // Fill bytes before a marker, and the markers that carry no length.
        if marker == 0xFF {
            rest = &rest[1..];
            continue;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            rest = after;
            continue;
        }
        let (length, body) = after.split_first_chunk::<2>()?;
        let length = usize::from(u16::from_be_bytes(*length));
        // SOF0..SOF15 are frame headers, except the three that are tables.
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let field = |at: usize| {
                Some(u32::from(u16::from_be_bytes([
                    *body.get(at)?,
                    *body.get(at + 1)?,
                ])))
            };
            return Some((field(3)?, field(1)?));
        }
        // Start of scan or end of image: no frame header came first.
        if matches!(marker, 0xDA | 0xD9) {
            return None;
        }
        rest = after.get(length.max(2)..)?;
    }
}

/// The largest centred part of a `source`-sized picture with `target`'s
/// aspect ratio: scaled to `target` it covers the screen without distortion.
/// `None` when either size is empty.
pub fn cover(source: (u32, u32), target: (u32, u32)) -> Option<Rect> {
    let (sw, sh) = (u64::from(source.0), u64::from(source.1));
    let (tw, th) = (u64::from(target.0), u64::from(target.1));
    if sw == 0 || sh == 0 || tw == 0 || th == 0 {
        return None;
    }
    // Wider than the screen: trim the sides; taller: trim top and bottom.
    let (w, h) = if sw * th > sh * tw {
        ((sh * tw / th).max(1), sh)
    } else {
        (sw, (sw * th / tw).max(1))
    };
    Some(Rect::new(
        ((sw - w) / 2) as i32,
        ((sh - h) / 2) as i32,
        w as i32,
        h as i32,
    ))
}

/// The mean colour (`0xRRGGBB`) of `region` in RGBA `pixels`, `width` pixels
/// per row, sampling every `step`th pixel each way. `None` when the region
/// holds no pixel of the picture.
pub fn mean_rgb(pixels: &[u8], width: usize, region: Rect, step: usize) -> Option<u32> {
    let step = step.max(1);
    let height = pixels.len() / 4 / width.max(1);
    let clamp = |value: i32, max: usize| (value.max(0) as usize).min(max);
    let (left, right) = (clamp(region.x, width), clamp(region.x + region.w, width));
    let (top, bottom) = (clamp(region.y, height), clamp(region.y + region.h, height));
    let (mut sum, mut count) = ([0u64; 3], 0u64);
    for y in (top..bottom).step_by(step) {
        for x in (left..right).step_by(step) {
            let at = (y * width + x) * 4;
            for (total, channel) in sum.iter_mut().zip(&pixels[at..at + 3]) {
                *total += u64::from(*channel);
            }
            count += 1;
        }
    }
    (count > 0).then(|| {
        let mean = |total: u64| (total / count) as u32;
        (mean(sum[0]) << 16) | (mean(sum[1]) << 8) | mean(sum[2])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = PNG_HEAD.to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    /// SOI, an APP0 segment, a quantisation table, then `frame`.
    fn jpeg(frame: u8, width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F'];
        bytes.extend_from_slice(&[0xFF, 0xDB, 0, 3, 0, 0xFF]);
        bytes.extend_from_slice(&[0xFF, frame, 0, 8, 8]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.push(3);
        bytes
    }

    #[test]
    fn png_and_jpeg_headers_give_their_size() {
        assert_eq!(dimensions(&png(2560, 1440)), Some((2560, 1440)));
        assert_eq!(dimensions(&png(1, u32::MAX)), Some((1, u32::MAX)));
        assert_eq!(dimensions(&jpeg(0xC0, 1920, 1080)), Some((1920, 1080)));
        // Progressive frames count too.
        assert_eq!(dimensions(&jpeg(0xC2, 640, 480)), Some((640, 480)));
    }

    #[test]
    fn other_and_truncated_files_have_no_size() {
        assert_eq!(dimensions(b""), None);
        assert_eq!(dimensions(b"GIF89a"), None);
        assert_eq!(dimensions(&png(8, 8)[..20]), None);
        let whole = jpeg(0xC0, 8, 8);
        // (The last byte, the component count, is past the size fields.)
        for cut in 2..whole.len() - 1 {
            assert_eq!(dimensions(&whole[..cut]), None, "cut at {cut}");
        }
        // A Huffman table (0xC4) is not a frame header.
        assert_eq!(dimensions(&jpeg(0xC4, 8, 8)), None);
        // A scan before any frame, and a zero segment length, both end it.
        assert_eq!(dimensions(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]), None);
        assert_eq!(
            dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0xFF, 0xE0]),
            None
        );
    }

    #[test]
    fn cover_keeps_the_target_aspect_and_stays_centred() {
        // Same shape: the whole picture.
        assert_eq!(
            cover((2560, 1440), (1280, 720)),
            Some(Rect::new(0, 0, 2560, 1440))
        );
        // A 4:3 photo on a 16:9 screen loses its top and bottom.
        assert_eq!(
            cover((1600, 1200), (1280, 720)),
            Some(Rect::new(0, 150, 1600, 900))
        );
        // An ultra-wide picture loses its sides.
        assert_eq!(
            cover((3000, 1000), (1000, 1000)),
            Some(Rect::new(1000, 0, 1000, 1000))
        );
        // A sliver still yields one pixel, never an empty crop.
        assert_eq!(cover((1, 5000), (5000, 1)), Some(Rect::new(0, 2499, 1, 1)));
        assert_eq!(cover((0, 10), (10, 10)), None);
        assert_eq!(cover((10, 10), (10, 0)), None);
    }

    #[test]
    fn mean_rgb_averages_the_region_and_clips_it() {
        // 4x2: a dark left half, a white right half.
        let mut pixels = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 0x10 } else { 0xFF };
                pixels.extend_from_slice(&[v, v, v, 0xFF]);
            }
        }
        assert_eq!(
            mean_rgb(&pixels, 4, Rect::new(0, 0, 2, 2), 1),
            Some(0x101010)
        );
        assert_eq!(
            mean_rgb(&pixels, 4, Rect::new(2, 0, 50, 50), 1),
            Some(0xFFFFFF)
        );
        assert_eq!(
            mean_rgb(&pixels, 4, Rect::new(1, 0, 2, 1), 1),
            Some(0x878787)
        );
        assert_eq!(
            mean_rgb(&pixels, 4, Rect::new(-3, -3, 4, 4), 7),
            Some(0x101010)
        );
        assert_eq!(mean_rgb(&pixels, 4, Rect::new(4, 0, 2, 2), 1), None);
        assert_eq!(mean_rgb(&pixels, 0, Rect::new(0, 0, 2, 2), 1), None);
    }
}
