#![forbid(unsafe_code)]

//! The cross-platform, software-rendered backend for xui.
//!
//! It rasterises the portable widgets with `tiny-skia` into an RGBA buffer, so
//! the same widget code that runs on the Win32 backend draws here without any
//! platform UI toolkit. This crate currently provides the drawing surface and
//! an offscreen renderer; the windowing shell and event loop build on it.

mod canvas;
mod offscreen;
mod text;

pub use canvas::SkiaCanvas;
pub use offscreen::OffscreenBackend;
pub use text::measure as measure_text;
pub use text::set_default_font;

use tiny_skia::Pixmap;
use xui_core::color::Color;
use xui_core::geometry::Rect;

/// An RGBA image rendered by the canvas backend.
#[derive(Clone, Debug)]
pub struct RgbaImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Top-down RGBA pixels (row-major, 4 bytes each).
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// The pixel at `(x, y)`, if in bounds.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = ((y * self.width + x) * 4) as usize;
        self.pixels
            .get(at..at + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// A software surface: a rectangle being painted, filled from a background and
/// then drawn into with a [`SkiaCanvas`].
pub struct Surface {
    pixmap: Pixmap,
}

impl Surface {
    /// A transparent surface of `width` x `height` pixels.
    pub fn new(width: u32, height: u32) -> Surface {
        let mut pixmap = Pixmap::new(width.max(1), height.max(1)).expect("pixmap");
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        Surface { pixmap }
    }

    /// Fills the whole surface with `color`.
    pub fn fill(&mut self, color: Color) {
        self.pixmap.fill(to_skia(color));
    }

    /// Runs `draw` with a canvas over `bounds` (in surface coordinates),
    /// clipped to that rectangle, at a dots-per-inch of `dpi`.
    pub fn with_canvas_at<R>(
        &mut self,
        bounds: Rect,
        dpi: u32,
        draw: impl FnOnce(&mut SkiaCanvas) -> R,
    ) -> R {
        let mut canvas = SkiaCanvas::new(&mut self.pixmap, bounds, dpi);
        draw(&mut canvas)
    }

    /// Runs `draw` with a canvas at a dots-per-inch of 96.
    pub fn with_canvas<R>(&mut self, bounds: Rect, draw: impl FnOnce(&mut SkiaCanvas) -> R) -> R {
        self.with_canvas_at(bounds, 96, draw)
    }

    /// Reads the surface back as an [`RgbaImage`].
    pub fn to_image(&self) -> RgbaImage {
        RgbaImage {
            width: self.pixmap.width(),
            height: self.pixmap.height(),
            pixels: self.pixmap.data().to_vec(),
        }
    }
}

/// Converts a core [`Color`] to a tiny-skia colour.
pub(crate) fn to_skia(color: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(color.r, color.g, color.b, 255)
}

#[cfg(test)]
mod tests;
