#![forbid(unsafe_code)]

//! Windows bitmaps (`.bmp`).
//!
//! Reads uncompressed 1, 4, 8 (palette), 24 and 32 bit files with a core,
//! `BITMAPINFOHEADER`, V4 or V5 header, top-down or bottom-up, and 32 bit
//! `BI_BITFIELDS` with any channel masks. Writes 24 bit when the picture is
//! fully opaque (what every tool opens), else a 32 bit `BITMAPV4HEADER` with
//! an alpha mask. RLE and 16 bit files are refused with a message.
//!
//! The reader is for untrusted files: the size is bounded by the memory
//! budget, and the pixel data must really be in the file before a buffer is
//! allocated for it, so a tiny file cannot claim a huge picture.

use super::{HISTORY_RESERVE, png_size_ok};
use crate::model::bitmap::{Bitmap, MAX_SIDE};
use crate::model::required_bytes;

const FILE_HEADER: usize = 14;
const INFO_HEADER: usize = 40;
const V4_HEADER: usize = 108;
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;
/// `LCS_sRGB`, the colour space tag of a V4 header.
const SRGB: u32 = 0x7352_4742;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The parsed header: everything needed to find and decode the pixels.
struct Header {
    width: u32,
    height: u32,
    top_down: bool,
    bpp: u16,
    compression: u32,
    /// Where the palette starts and how many entries (bytes each) it has.
    palette: (usize, usize, usize),
    /// The R, G, B, A channel masks of 32 bit files.
    masks: [u32; 4],
    data_at: usize,
}

fn header(b: &[u8]) -> Result<Header, String> {
    let short = || "the BMP file is truncated".to_string();
    if b.get(..2) != Some(b"BM") {
        return Err("not a BMP file".to_string());
    }
    let data_at = u32_at(b, 10).ok_or_else(short)? as usize;
    let dib = u32_at(b, FILE_HEADER).ok_or_else(short)? as usize;
    let (width, height, bpp, compression, colors, entry) = if dib == 12 {
        let w = i32::from(u16_at(b, 18).ok_or_else(short)?);
        let h = i32::from(u16_at(b, 20).ok_or_else(short)?);
        (w, h, u16_at(b, 24).ok_or_else(short)?, BI_RGB, 0, 3)
    } else if (INFO_HEADER..=V4_HEADER + 16).contains(&dib) {
        let w = u32_at(b, 18).ok_or_else(short)? as i32;
        let h = u32_at(b, 22).ok_or_else(short)? as i32;
        let bpp = u16_at(b, 28).ok_or_else(short)?;
        let compression = u32_at(b, 30).ok_or_else(short)?;
        (
            w,
            h,
            bpp,
            compression,
            u32_at(b, 46).ok_or_else(short)? as usize,
            4,
        )
    } else {
        return Err(format!("unsupported BMP header size {dib}"));
    };
    if width <= 0 || height == 0 {
        return Err("the BMP has no size".to_string());
    }
    let (width, height) = (width as u32, height.unsigned_abs());
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("the BMP is too large ({width} x {height})"));
    }
    if u16_at(b, if dib == 12 { 22 } else { 26 }) != Some(1) {
        return Err("the BMP is malformed".to_string());
    }
    match (bpp, compression) {
        (1 | 4 | 8 | 24, BI_RGB) | (32, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) => {}
        (_, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) => {
            return Err(format!("{bpp} bits per pixel is not supported"));
        }
        _ => return Err("compressed BMP files are not supported".to_string()),
    }
    let mut masks = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000];
    if compression != BI_RGB {
        let at = FILE_HEADER + INFO_HEADER;
        for (n, mask) in masks.iter_mut().enumerate().take(3) {
            *mask = u32_at(b, at + n * 4).ok_or_else(short)?;
        }
        masks[3] = if dib >= 56 || compression == BI_ALPHABITFIELDS {
            u32_at(b, at + 12).ok_or_else(short)?
        } else {
            0
        };
    }
    let colors = if colors == 0 && bpp <= 8 {
        1 << bpp
    } else {
        colors.min(256)
    };
    Ok(Header {
        width,
        height,
        top_down: u32_at(b, 22).is_some_and(|h| (h as i32) < 0) && dib != 12,
        bpp,
        compression,
        palette: (FILE_HEADER + dib, if bpp <= 8 { colors } else { 0 }, entry),
        masks,
        data_at,
    })
}

/// A channel of `px` selected by `mask`, scaled to 0..=255.
fn channel(px: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    // Masks are contiguous runs of at most 32 bits; a hostile one that is not
    // still yields a value in range.
    let value = u64::from((px & mask) >> shift).min(max);
    ((value * 255 + max / 2) / max) as u8
}

/// Reads a BMP into a bitmap.
pub fn read(b: &[u8]) -> Result<Bitmap, String> {
    let h = header(b)?;
    let (w, ht) = (h.width as usize, h.height as usize);
    // Reserve the document, the undo cache and the transient decode buffer.
    let held = required_bytes(h.width, h.height, 1).saturating_add(HISTORY_RESERVE.total());
    if !png_size_ok(h.width, h.height, held) {
        return Err("the BMP is too large for the memory budget".to_string());
    }
    let stride = (w as u64 * u64::from(h.bpp)).div_ceil(32) * 4;
    let need = stride * ht as u64;
    let data = usize::try_from(need)
        .ok()
        .and_then(|n| h.data_at.checked_add(n))
        .and_then(|end| b.get(h.data_at..end))
        .ok_or("the BMP is truncated")?;
    let stride = stride as usize;
    let (pal_at, pal_len, entry) = h.palette;
    let palette: Vec<[u8; 4]> = (0..pal_len)
        .map(|i| {
            let at = pal_at + i * entry;
            b.get(at..at + 3)
                .map_or([0, 0, 0, 255], |c| [c[2], c[1], c[0], 255])
        })
        .collect();
    let any_alpha =
        h.bpp == 32 && h.compression == BI_RGB && data.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
    let mut out = Bitmap::transparent(h.width, h.height);
    let dst = out.pixels_mut().as_chunks_mut::<4>().0;
    for y in 0..ht {
        let row = &data[y * stride..(y + 1) * stride];
        let to = if h.top_down { y } else { ht - 1 - y };
        let line = &mut dst[to * w..(to + 1) * w];
        for (x, px) in line.iter_mut().enumerate() {
            *px = match h.bpp {
                24 => [row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255],
                32 => {
                    let v = u32::from_le_bytes([
                        row[x * 4],
                        row[x * 4 + 1],
                        row[x * 4 + 2],
                        row[x * 4 + 3],
                    ]);
                    let a = if h.compression == BI_RGB {
                        if any_alpha { row[x * 4 + 3] } else { 255 }
                    } else if h.masks[3] == 0 {
                        255
                    } else {
                        channel(v, h.masks[3])
                    };
                    [
                        channel(v, h.masks[0]),
                        channel(v, h.masks[1]),
                        channel(v, h.masks[2]),
                        a,
                    ]
                }
                bpp => {
                    let bits = usize::from(bpp);
                    let at = x * bits;
                    let byte = row[at / 8];
                    let index = (byte >> (8 - bits - at % 8)) & (((1u16 << bits) - 1) as u8);
                    palette
                        .get(usize::from(index))
                        .copied()
                        .unwrap_or([0, 0, 0, 255])
                }
            };
        }
    }
    Ok(out)
}

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Writes a bitmap: 24 bit when fully opaque, else 32 bit with alpha in a
/// `BITMAPV4HEADER`.
pub fn write(bitmap: &Bitmap) -> Result<Vec<u8>, String> {
    let opaque = bitmap
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|p| p[3] == 255);
    let (w, h) = (bitmap.width() as usize, bitmap.height() as usize);
    let (bpp, dib) = if opaque {
        (24u16, INFO_HEADER)
    } else {
        (32, V4_HEADER)
    };
    let stride = (w * usize::from(bpp)).div_ceil(32) * 4;
    let image = stride * h;
    let total = FILE_HEADER + dib + image;
    let too_big = || "the picture is too large for a BMP file".to_string();
    let total32 = u32::try_from(total).map_err(|_| too_big())?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"BM");
    push32(&mut out, total32);
    push32(&mut out, 0);
    push32(&mut out, (FILE_HEADER + dib) as u32);
    push32(&mut out, dib as u32);
    push32(&mut out, w as u32);
    push32(&mut out, h as u32);
    push16(&mut out, 1);
    push16(&mut out, bpp);
    push32(&mut out, if opaque { BI_RGB } else { BI_BITFIELDS });
    push32(&mut out, image as u32);
    push32(&mut out, 2835);
    push32(&mut out, 2835);
    push32(&mut out, 0);
    push32(&mut out, 0);
    if !opaque {
        for mask in [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000] {
            push32(&mut out, mask);
        }
        push32(&mut out, SRGB);
        out.resize(out.len() + 36 + 12, 0);
    }
    for y in (0..h).rev() {
        let row = bitmap.row(y as i32, 0, w);
        let start = out.len();
        for p in row.as_chunks::<4>().0 {
            if opaque {
                out.extend_from_slice(&[p[2], p[1], p[0]]);
            } else {
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        out.resize(start + stride, 0);
    }
    Ok(out)
}
