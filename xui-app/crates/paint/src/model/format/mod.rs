#![forbid(unsafe_code)]

//! File formats (P6): which one a file is, reading it into a [`Document`], and
//! writing a document out.
//!
//! Everything here works on bytes, so it goes through any
//! [`Storage`](crate::storage::Storage) (`save_to`/`load_from` carry bytes, not
//! a format) and does no I/O itself. Opening finds the format from the
//! *contents* (magic bytes), never the file name; saving picks it from the
//! name's extension.
//!
//! | format | open | save | layers |
//! |---|---|---|---|
//! | OpenRaster `.ora` | yes | yes | yes, with name, opacity, visibility |
//! | PNG `.png` | yes | yes | no: flattened |
//! | BMP `.bmp` | yes | yes | no: flattened |
//! | JPEG `.jpg` `.jpeg` | yes | no | no |

mod bmp;
mod inflate;
mod ora;
mod xml;
mod zip;

#[cfg(test)]
mod bmp_tests;
#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod tests;

use std::path::Path;

use xui_core::image::Image;

use super::bitmap::Bitmap;
use super::history::DEFAULT_CAP;
use super::image::{Document, MEMORY_BUDGET, Reserved};

/// What the open paths leave room for: the history's default cap.
pub(crate) const HISTORY_RESERVE: Reserved = Reserved {
    floating: 0,
    clipboard: 0,
    history: DEFAULT_CAP as u64,
};

/// A file format Paint knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// OpenRaster, the native layered format.
    OpenRaster,
    /// Portable Network Graphics.
    Png,
    /// Windows bitmap.
    Bmp,
    /// JPEG (open only).
    Jpeg,
}

impl Format {
    /// The name shown in dialogs and markers ("OpenRaster", "PNG").
    pub fn name(self) -> &'static str {
        match self {
            Format::OpenRaster => "OpenRaster",
            Format::Png => "PNG",
            Format::Bmp => "BMP",
            Format::Jpeg => "JPEG",
        }
    }

    /// The preferred extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::OpenRaster => "ora",
            Format::Png => "png",
            Format::Bmp => "bmp",
            Format::Jpeg => "jpg",
        }
    }

    /// The MIME type.
    pub fn mime(self) -> &'static str {
        match self {
            Format::OpenRaster => "image/openraster",
            Format::Png => "image/png",
            Format::Bmp => "image/bmp",
            Format::Jpeg => "image/jpeg",
        }
    }

    /// Whether a file of this format keeps layers.
    pub fn keeps_layers(self) -> bool {
        self == Format::OpenRaster
    }

    /// Whether Paint can write this format.
    pub fn can_save(self) -> bool {
        self != Format::Jpeg
    }

    /// The format a file name's extension names (case-insensitive).
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "ora" => Some(Format::OpenRaster),
            "png" => Some(Format::Png),
            "bmp" | "dib" => Some(Format::Bmp),
            "jpg" | "jpeg" | "jpe" => Some(Format::Jpeg),
            _ => None,
        }
    }

    /// The format the leading bytes of a file show, if it is one of ours.
    pub fn detect(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Png)
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Format::Jpeg)
        } else if bytes.starts_with(b"BM") {
            Some(Format::Bmp)
        } else if bytes.starts_with(b"PK\x03\x04") {
            // The only zip we read; `ora::read` checks the mimetype.
            Some(Format::OpenRaster)
        } else {
            None
        }
    }
}

/// Reads a file's bytes into a document, finding the format from the bytes.
/// The error is a short message for the status bar.
pub fn open(bytes: &[u8]) -> Result<(Format, Document), String> {
    let format = Format::detect(bytes).ok_or("this is not a picture Paint can open")?;
    let doc = match format {
        Format::OpenRaster => ora::read(bytes)?,
        Format::Bmp => Document::from_bitmap(bmp::read(bytes)?).map_err(|e| e.to_string())?,
        Format::Png | Format::Jpeg => {
            Document::from_bitmap(decode_picture(bytes)?).map_err(|e| e.to_string())?
        }
    };
    Ok((format, doc))
}

/// Decodes a PNG or JPEG after checking the size its header declares, since
/// the decoder allocates the whole picture from that header before reading a
/// pixel. Every untrusted PNG or JPEG (a file, the system clipboard) goes
/// through here.
pub fn decode_picture(bytes: &[u8]) -> Result<Bitmap, String> {
    let (w, h) = sniff_size(bytes).ok_or("the picture is damaged")?;
    if !png_size_ok(w, h, HISTORY_RESERVE.total()) {
        return Err(format!("the picture is too large ({w} x {h})"));
    }
    let image = Image::decode(bytes).map_err(|e| format!("the picture is damaged: {e}"))?;
    Ok(Bitmap::from_image(&image))
}

/// Encodes `doc` as `format`. The document is never changed: a format
/// without layers gets the visible layers composited into a copy.
pub fn save(format: Format, doc: &Document) -> Result<Vec<u8>, String> {
    match format {
        Format::OpenRaster => ora::write(doc),
        Format::Png => doc.flatten().encode_png().map_err(|e| e.to_string()),
        Format::Bmp => bmp::write(&doc.flatten()),
        Format::Jpeg => {
            Err("Paint cannot save JPEG files; save as PNG, BMP or OpenRaster".to_string())
        }
    }
}

/// Whether saving `doc` as `format` would lose layers, so the UI should ask
/// "Flatten image?" first. A one-layer document never needs it.
pub fn needs_flatten(format: Format, doc: &Document) -> bool {
    !format.keeps_layers() && doc.layers().len() > 1
}

/// The pixel size a PNG or JPEG header declares, read without decoding.
pub(crate) fn sniff_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return (bytes.get(12..16)? == b"IHDR").then_some((be32(16)?, be32(20)?));
    }
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    // Walk the marker segments to the first start-of-frame.
    let mut at = 2;
    loop {
        while bytes.get(at)? != &0xFF {
            at += 1;
        }
        while bytes.get(at)? == &0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let h = u32::from(u16::from_be_bytes(
                    bytes.get(at + 3..at + 5)?.try_into().ok()?,
                ));
                let w = u32::from(u16::from_be_bytes(
                    bytes.get(at + 5..at + 7)?.try_into().ok()?,
                ));
                return Some((w, h));
            }
            0xD9 | 0xDA => return None,
            _ => {
                let len = usize::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
                at += len.max(2);
            }
        }
    }
}

/// Whether decoding a `width` x `height` picture, with `held` bytes already
/// spoken for, stays inside the memory budget. Decoding builds the picture
/// and then copies it into a bitmap, so it counts twice.
pub(crate) fn png_size_ok(width: u32, height: u32, held: u64) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let one = u64::from(width) * u64::from(height) * 4;
    one.saturating_mul(2).saturating_add(held) <= MEMORY_BUDGET
}
