//! The 1796-byte page header (PWG 5102.4 4.3, CUPS's `cups_page_header2_t`
//! layout), big-endian.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The header's size in bytes.
pub const HEADER_LEN: usize = 1796;

// Byte offsets of the fields written or read.
const MEDIA_CLASS: usize = 0;
const PRINT_CONTENT_OPTIMIZE: usize = 192;
const HW_RESOLUTION: usize = 276;
const NUM_COPIES: usize = 340;
const PAGE_SIZE: usize = 352;
const WIDTH: usize = 372;
const HEIGHT: usize = 376;
const BITS_PER_COLOR: usize = 384;
const BITS_PER_PIXEL: usize = 388;
const BYTES_PER_LINE: usize = 392;
const COLOR_ORDER: usize = 396;
const COLOR_SPACE: usize = 400;
const NUM_COLORS: usize = 420;
const TOTAL_PAGE_COUNT: usize = 452;
const CROSS_FEED_TRANSFORM: usize = 456;
const FEED_TRANSFORM: usize = 460;
const IMAGE_BOX: usize = 464;
const ALTERNATE_PRIMARY: usize = 480;
const PRINT_QUALITY: usize = 484;
const RENDERING_INTENT: usize = 1668;
const PAGE_SIZE_NAME: usize = 1732;
/// The length of a string field, NUL terminator included.
const STRING_LEN: usize = 64;

/// The colour spaces this encoder writes, 8 bits per colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    /// `sgray_8`: one byte per pixel, 0 black to 255 white.
    Sgray8,
    /// `srgb_8`: three bytes per pixel.
    Srgb8,
}

impl ColorSpace {
    /// Bytes per pixel.
    pub fn bytes(self) -> usize {
        match self {
            ColorSpace::Sgray8 => 1,
            ColorSpace::Srgb8 => 3,
        }
    }

    /// The header's `cupsColorSpace` value (`CUPS_CSPACE_SW`, `CUPS_CSPACE_SRGB`).
    fn code(self) -> u32 {
        match self {
            ColorSpace::Sgray8 => 18,
            ColorSpace::Srgb8 => 19,
        }
    }

    fn from_code(code: u32, bits_per_pixel: u32) -> Option<ColorSpace> {
        match (code, bits_per_pixel) {
            (18, 8) => Some(ColorSpace::Sgray8),
            (19, 24) => Some(ColorSpace::Srgb8),
            _ => None,
        }
    }
}

/// What a page header says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Dots per inch, the same across and down.
    pub dpi: u32,
    pub color: ColorSpace,
    /// The PWG media name, e.g. `iso_a4_210x297mm` (at most 63 bytes).
    pub media: String,
    /// `print-quality` (3, 4 or 5), 0 for the printer's default.
    pub quality: u32,
    /// Pages in the stream, 0 when not known up front.
    pub total_pages: u32,
}

impl Header {
    /// Bytes per row.
    pub fn bytes_per_line(&self) -> usize {
        self.width as usize * self.color.bytes()
    }

    /// The header's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        let mut put = |at: usize, value: u32| h[at..at + 4].copy_from_slice(&value.to_be_bytes());
        let bits = 8 * self.color.bytes() as u32;
        // PageSize is in points, rounded.
        let points = |px: u32| {
            ((u64::from(px) * 72 + u64::from(self.dpi) / 2) / u64::from(self.dpi.max(1))) as u32
        };
        put(HW_RESOLUTION, self.dpi);
        put(HW_RESOLUTION + 4, self.dpi);
        put(NUM_COPIES, 1);
        put(PAGE_SIZE, points(self.width));
        put(PAGE_SIZE + 4, points(self.height));
        put(WIDTH, self.width);
        put(HEIGHT, self.height);
        put(BITS_PER_COLOR, 8);
        put(BITS_PER_PIXEL, bits);
        put(BYTES_PER_LINE, self.bytes_per_line() as u32);
        put(COLOR_ORDER, 0);
        put(COLOR_SPACE, self.color.code());
        put(NUM_COLORS, self.color.bytes() as u32);
        put(TOTAL_PAGE_COUNT, self.total_pages);
        put(CROSS_FEED_TRANSFORM, 1);
        put(FEED_TRANSFORM, 1);
        put(IMAGE_BOX + 8, self.width);
        put(IMAGE_BOX + 12, self.height);
        put(ALTERNATE_PRIMARY, 0x00FF_FFFF);
        put(PRINT_QUALITY, self.quality);
        string(&mut h, MEDIA_CLASS, "PwgRaster");
        string(&mut h, PRINT_CONTENT_OPTIMIZE, "text");
        string(&mut h, RENDERING_INTENT, "perceptual");
        string(&mut h, PAGE_SIZE_NAME, &self.media);
        h
    }

    /// Reads a header; `None` for a colour space this crate does not write or
    /// a row length that disagrees with the width.
    pub fn from_bytes(h: &[u8; HEADER_LEN]) -> Option<Header> {
        let get = |at: usize| u32::from_be_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
        if get(BITS_PER_COLOR) != 8 || get(COLOR_ORDER) != 0 {
            return None;
        }
        let color = ColorSpace::from_code(get(COLOR_SPACE), get(BITS_PER_PIXEL))?;
        let header = Header {
            width: get(WIDTH),
            height: get(HEIGHT),
            dpi: get(HW_RESOLUTION),
            color,
            media: read_string(&h[PAGE_SIZE_NAME..PAGE_SIZE_NAME + STRING_LEN]),
            quality: get(PRINT_QUALITY),
            total_pages: get(TOTAL_PAGE_COUNT),
        };
        let line = u64::from(header.width) * color.bytes() as u64;
        (line == u64::from(get(BYTES_PER_LINE))).then_some(header)
    }
}

/// Writes `value` NUL-terminated, cut to fit.
fn string(h: &mut [u8], at: usize, value: &str) {
    let bytes = &value.as_bytes()[..value.len().min(STRING_LEN - 1)];
    h[at..at + bytes.len()].copy_from_slice(bytes);
}

fn read_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}
