//! Linear framebuffer abstraction with format-aware pixel access.

use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::ptr;

/// An RGB colour.
#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b }
    }
}

/// A raw linear framebuffer handed over by the bootloader.
pub struct Framebuffer {
    base: usize,
    info: FrameBufferInfo,
}

impl Framebuffer {
    pub fn new(base: usize, info: FrameBufferInfo) -> Self {
        Framebuffer { base, info }
    }

    pub fn width(&self) -> usize {
        self.info.width
    }

    pub fn height(&self) -> usize {
        self.info.height
    }

    pub fn clear(&mut self, color: Color) {
        for y in 0..self.height() {
            for x in 0..self.width() {
                self.write_pixel(x, y, color);
            }
        }
    }

    /// Byte offset of a pixel. `stride` is measured in pixels, so it must be
    /// scaled by the bytes-per-pixel to address a row.
    fn offset(&self, x: usize, y: usize) -> usize {
        (y * self.info.stride + x) * self.info.bytes_per_pixel
    }

    fn ptr_at(&self, x: usize, y: usize) -> *mut u8 {
        (self.base as *mut u8).wrapping_add(self.offset(x, y))
    }

    /// Write an opaque pixel, honouring RGB/BGR and 3/4 bytes per pixel.
    pub fn write_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.width() || y >= self.height() {
            return;
        }
        let (b0, b1, b2) = match self.info.pixel_format {
            PixelFormat::Bgr => (color.b, color.g, color.r),
            _ => (color.r, color.g, color.b),
        };
        let p = self.ptr_at(x, y);
        // Safety: bounds were checked; the framebuffer is a valid mapped region.
        unsafe {
            ptr::write_volatile(p, b0);
            ptr::write_volatile(p.add(1), b1);
            ptr::write_volatile(p.add(2), b2);
            if self.info.bytes_per_pixel == 4 {
                ptr::write_volatile(p.add(3), 0xFF);
            }
        }
    }

    /// Read a pixel back from the framebuffer.
    pub fn read_pixel(&self, x: usize, y: usize) -> Color {
        if x >= self.width() || y >= self.height() {
            return Color::rgb(0, 0, 0);
        }
        let p = self.ptr_at(x, y);
        // Safety: bounds were checked.
        let (b0, b1, b2) = unsafe {
            (
                ptr::read_volatile(p),
                ptr::read_volatile(p.add(1)),
                ptr::read_volatile(p.add(2)),
            )
        };
        match self.info.pixel_format {
            PixelFormat::Bgr => Color::rgb(b2, b1, b0),
            _ => Color::rgb(b0, b1, b2),
        }
    }

    /// Fast path for presenting an opaque RGBA image (4 bytes/pixel, e.g. a
    /// tiny-skia `Pixmap`) over the whole framebuffer. Writes are non-volatile
    /// (the framebuffer is ordinary RAM on x86) and the pixel format is
    /// resolved once, which is far faster than per-pixel `write_pixel`.
    pub fn blit_rgba(&mut self, rgba: &[u8], width: usize, height: usize) {
        let bpp = self.info.bytes_per_pixel;
        let stride = self.info.stride;
        let w = width.min(self.width());
        let h = height.min(self.height());
        let bgr = matches!(self.info.pixel_format, PixelFormat::Bgr);
        let base = self.base as *mut u8;

        for y in 0..h {
            let src = &rgba[y * width * 4..];
            // Safety: row y is within the framebuffer and `w` is clamped.
            let row = unsafe { base.add(y * stride * bpp) };
            for x in 0..w {
                let i = x * 4;
                let (r, g, b) = (src[i], src[i + 1], src[i + 2]);
                let (b0, b1, b2) = if bgr { (b, g, r) } else { (r, g, b) };
                let p = unsafe { row.add(x * bpp) };
                // Safety: within the row, and bpp is 3 or 4.
                unsafe {
                    p.write(b0);
                    p.add(1).write(b1);
                    p.add(2).write(b2);
                    if bpp == 4 {
                        p.add(3).write(0xFF);
                    }
                }
            }
        }
    }

    /// Alpha-blend `fg` over the existing pixel using 8-bit coverage.
    pub fn blend_pixel(&mut self, x: usize, y: usize, fg: Color, coverage: u8) {
        if coverage == 0 {
            return;
        }
        let bg = self.read_pixel(x, y);
        let blend = |f: u8, b: u8| -> u8 {
            // out = b + (f - b) * coverage / 255
            let diff = f as i32 - b as i32;
            (b as i32 + diff * coverage as i32 / 255) as u8
        };
        let out = Color::rgb(blend(fg.r, bg.r), blend(fg.g, bg.g), blend(fg.b, bg.b));
        self.write_pixel(x, y, out);
    }

    /// Scroll the whole framebuffer up by `lines` text rows and clear the
    /// revealed area at the bottom.
    pub fn scroll_up(&mut self, lines: usize, clear: Color) {
        let row_bytes = self.info.stride * self.info.bytes_per_pixel;
        let height = self.height();
        if lines == 0 || lines >= height {
            self.clear(clear);
            return;
        }
        let shift = lines * row_bytes;
        // Safety: copying within the mapped framebuffer region.
        unsafe {
            let dst = self.base as *mut u8;
            let src = dst.add(shift);
            ptr::copy(src, dst, (height - lines) * row_bytes);
        }
        for y in (height - lines)..height {
            for x in 0..self.width() {
                self.write_pixel(x, y, clear);
            }
        }
    }
}
