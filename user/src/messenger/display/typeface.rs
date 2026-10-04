//! Proportional anti-aliased text for the compositor chrome.
//!
//! [`Face::Sans`] (Droid Sans) and [`Face::Serif`] (Droid Serif) are
//! rasterized at build time by `user/build.rs` (via `font-atlas`) into 8-bit
//! coverage atlases; see `assets/fonts/` and the README credits. Unlike the 5x7
//! [`font`](super::font) these have per-glyph advances and lower case.

use core::sync::atomic::{AtomicU32, Ordering};

/// Metrics and coverage location of one glyph (mirrors `font_atlas::Glyph`).
pub struct Glyph {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub offset: u32,
    /// Advance to the next pen position, in 1/16 pixel.
    pub advance_x16: u32,
}

/// One rasterized face: line metrics plus glyphs for ASCII and Latin-1.
pub struct FaceData {
    pub ascender: i32,
    pub descender: i32,
    pub line_height: i32,
    pub glyphs: &'static [Glyph],
    pub coverage: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/typeface_data.rs"));

/// The UI scale the faces draw at (1 or 2; docs/hidpi-plan.md). Each scale
/// has its own atlas rasterized at that size, so every metric below is
/// already in physical pixels.
static SCALE: AtomicU32 = AtomicU32::new(1);

/// Draw every face at `scale` (clamped to the atlases built: 1 or 2).
pub fn set_scale(scale: u32) {
    SCALE.store(scale.clamp(1, 2), Ordering::Relaxed);
}

const FIRST: u32 = 0x20;
/// Matches `font_atlas::LAST_CHAR`: the atlas covers ASCII and Latin-1.
const LAST: u32 = 0xFF;

/// A bundled typeface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    /// Droid Sans: window titles, taskbar, menus.
    Sans,
    /// Droid Serif: headings and placeholders.
    Serif,
}

impl Face {
    fn data(self) -> &'static FaceData {
        match (self, SCALE.load(Ordering::Relaxed)) {
            (Face::Sans, 2) => &SANS_2X,
            (Face::Serif, 2) => &SERIF_2X,
            (Face::Sans, _) => &SANS_1X,
            (Face::Serif, _) => &SERIF_1X,
        }
    }

    /// The glyph for `ch` and its coverage; characters outside the atlas
    /// (ASCII and Latin-1) draw as `?`.
    pub(super) fn glyph(self, ch: char) -> (&'static Glyph, &'static [u8]) {
        let data = self.data();
        let code = ch as u32;
        let code = if (FIRST..=LAST).contains(&code) {
            code
        } else {
            '?' as u32
        };
        let glyph = &data.glyphs[(code - FIRST) as usize];
        let start = glyph.offset as usize;
        let len = (glyph.width * glyph.height) as usize;
        (glyph, &data.coverage[start..start + len])
    }

    /// Distance from the top of a line box to the baseline.
    pub fn ascent(self) -> i32 {
        self.data().ascender
    }

    /// Height of a line box (ascent plus descent), for vertical centring.
    pub fn height(self) -> i32 {
        self.data().ascender - self.data().descender
    }

    /// Suggested distance between consecutive baselines.
    pub fn line_height(self) -> i32 {
        self.data().line_height
    }

    /// Width of `text` in whole pixels (rounded up).
    pub fn width(self, text: &str) -> i32 {
        let sum: i32 = text
            .chars()
            .map(|ch| self.glyph(ch).0.advance_x16 as i32)
            .sum();
        (sum + 15) / 16
    }
}
