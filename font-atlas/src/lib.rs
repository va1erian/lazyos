//! Build-time glyph atlas generator.
//!
//! Rasterizes the printable ASCII and Latin-1 ranges of a real font into 8-bit
//! anti-aliased coverage bitmaps, plus the metrics needed to place them on a
//! baseline. The kernel console embeds a monospace atlas (`kernel/build.rs`);
//! the `xuid` compositor chrome embeds proportional ones (`user/build.rs`),
//! which is why every glyph also carries its own advance.

use fontdue::{Font, FontSettings};

/// First character included in the atlas (`' '`).
pub const FIRST_CHAR: u8 = 0x20;
/// Last character included in the atlas (U+00FF): ASCII plus Latin-1.
pub const LAST_CHAR: u8 = 0xFF;

/// Per-glyph layout and location in the coverage buffer.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal offset from the pen position to the bitmap's left edge.
    pub left: i32,
    /// Vertical offset from the baseline (y grows downward) to the bitmap top.
    pub top: i32,
    /// Byte offset of this glyph's bitmap within `Atlas::coverage`.
    pub offset: u32,
    /// Horizontal advance to the next pen position, in 1/16 pixel. Kept
    /// fractional so proportional text does not drift from rounding each glyph.
    pub advance_x16: u32,
}

/// A rasterized font atlas.
pub struct Atlas {
    /// Distance from the baseline to the top of the line (positive).
    pub ascender: i32,
    /// Distance from the baseline to the bottom of the line (negative).
    pub descender: i32,
    /// Recommended distance between consecutive baselines.
    pub line_height: i32,
    /// Fixed horizontal advance for every glyph (monospace faces only; use
    /// [`Glyph::advance_x16`] for proportional text).
    pub advance: u32,
    /// Metrics for `FIRST_CHAR..=LAST_CHAR`, in order.
    pub glyphs: Vec<Glyph>,
    /// Concatenated 8-bit coverage bitmaps (row-major, top-left origin).
    pub coverage: Vec<u8>,
}

/// Rasterize `font_bytes` at `px` pixels into an [`Atlas`].
pub fn build(font_bytes: &[u8], px: f32) -> Atlas {
    let font = Font::from_bytes(font_bytes, FontSettings::default()).expect("parse font");

    let line = font
        .horizontal_line_metrics(px)
        .expect("font has no horizontal line metrics");
    // A monospace font gives every glyph the same advance; use 'M' as the sample.
    let advance = font.metrics('M', px).advance_width.round().max(1.0) as u32;

    let mut glyphs = Vec::with_capacity((LAST_CHAR - FIRST_CHAR + 1) as usize);
    let mut coverage = Vec::new();

    for code in FIRST_CHAR..=LAST_CHAR {
        // DEL and the C1 controls have no glyph; keep their slots (indexing is
        // `code - FIRST_CHAR`) but draw nothing rather than a .notdef box.
        let (metrics, bitmap) = if (0x7F..=0x9F).contains(&code) {
            (fontdue::Metrics::default(), Vec::new())
        } else {
            font.rasterize(code as char, px)
        };
        let offset = coverage.len() as u32;
        // The bitmap is top-left origin; `ymin` is the offset (positive up) of
        // the bitmap's bottom edge from the baseline. Convert to a y-down
        // offset from the baseline to the bitmap's top edge.
        let top = -(metrics.ymin + metrics.height as i32);
        glyphs.push(Glyph {
            width: metrics.width as u32,
            height: metrics.height as u32,
            left: metrics.xmin,
            top,
            offset,
            advance_x16: (metrics.advance_width * 16.0).round().max(0.0) as u32,
        });
        coverage.extend_from_slice(&bitmap);
    }

    Atlas {
        ascender: line.ascent.round() as i32,
        descender: line.descent.round() as i32,
        line_height: line.new_line_size.round() as i32,
        advance,
        glyphs,
        coverage,
    }
}
