//! A compositing surface abstraction so the same drawing code can target the
//! live framebuffer or an off-screen RGBA back buffer (double buffering).

use crate::gfx::{Color, Framebuffer};
use alloc::vec::Vec;

/// Composite `fg` over `bg` using 8-bit coverage.
pub fn blend_over(fg: Color, bg: Color, coverage: u8) -> Color {
    let mix = |f: u8, b: u8| -> u8 {
        let diff = f as i32 - b as i32;
        (b as i32 + diff * coverage as i32 / 255) as u8
    };
    Color::rgb(mix(fg.r, bg.r), mix(fg.g, bg.g), mix(fg.b, bg.b))
}

/// A drawable pixel surface. Drawing code is generic over this so it can render
/// into the live framebuffer or an off-screen buffer before presenting.
pub trait Surface {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn get(&self, x: usize, y: usize) -> Color;
    fn set(&mut self, x: usize, y: usize, color: Color);

    /// Copy `w * h` pixels of an RGBA image from `(sx, sy)` to `(dx, dy)`.
    #[allow(clippy::too_many_arguments)]
    fn blit_rgba_region(
        &mut self,
        rgba: &[u8],
        src_w: usize,
        src_h: usize,
        sx: usize,
        sy: usize,
        dx: usize,
        dy: usize,
        w: usize,
        h: usize,
    );

    #[allow(dead_code)]
    fn blit_rgba_at(&mut self, rgba: &[u8], width: usize, height: usize, dx: usize, dy: usize) {
        self.blit_rgba_region(rgba, width, height, 0, 0, dx, dy, width, height);
    }

    fn blend_pixel(&mut self, x: usize, y: usize, fg: Color, coverage: u8) {
        if coverage == 0 || x >= self.width() || y >= self.height() {
            return;
        }
        let bg = self.get(x, y);
        self.set(x, y, blend_over(fg, bg, coverage));
    }

    fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        let (fw, fh) = (self.width() as i32, self.height() as i32);
        let (x0, y0) = (x0.max(0), y0.max(0));
        let (x1, y1) = (x1.min(fw), y1.min(fh));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                self.set(x as usize, y as usize, color);
            }
        }
    }
}

impl Surface for Framebuffer {
    fn width(&self) -> usize {
        Framebuffer::width(self)
    }
    fn height(&self) -> usize {
        Framebuffer::height(self)
    }
    fn get(&self, x: usize, y: usize) -> Color {
        self.read_pixel(x, y)
    }
    fn set(&mut self, x: usize, y: usize, color: Color) {
        self.write_pixel(x, y, color);
    }
    fn blit_rgba_region(
        &mut self,
        rgba: &[u8],
        src_w: usize,
        src_h: usize,
        sx: usize,
        sy: usize,
        dx: usize,
        dy: usize,
        w: usize,
        h: usize,
    ) {
        Framebuffer::blit_rgba_region(self, rgba, src_w, src_h, sx, sy, dx, dy, w, h);
    }
}

/// An off-screen RGBA (opaque) back buffer.
pub struct RgbaBuffer {
    w: usize,
    h: usize,
    data: Vec<u8>,
}

impl RgbaBuffer {
    /// A zeroed `w` x `h` buffer, or `None` (instead of stopping the kernel)
    /// when it does not fit: a screen-sized buffer is the heap's largest
    /// single allocation and may not fit a fragmented heap.
    pub fn try_new(w: usize, h: usize) -> Option<Self> {
        let bytes = w.checked_mul(h)?.checked_mul(4)?;
        let mut data = Vec::new();
        data.try_reserve_exact(bytes).ok()?;
        data.resize(bytes, 0);
        Some(RgbaBuffer { w, h, data })
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl Surface for RgbaBuffer {
    fn width(&self) -> usize {
        self.w
    }
    fn height(&self) -> usize {
        self.h
    }
    fn get(&self, x: usize, y: usize) -> Color {
        if x >= self.w || y >= self.h {
            return Color::rgb(0, 0, 0);
        }
        let i = (y * self.w + x) * 4;
        Color::rgb(self.data[i], self.data[i + 1], self.data[i + 2])
    }
    fn set(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = (y * self.w + x) * 4;
        self.data[i] = color.r;
        self.data[i + 1] = color.g;
        self.data[i + 2] = color.b;
        self.data[i + 3] = 0xFF;
    }
    fn blit_rgba_region(
        &mut self,
        rgba: &[u8],
        src_w: usize,
        src_h: usize,
        sx: usize,
        sy: usize,
        dx: usize,
        dy: usize,
        w: usize,
        h: usize,
    ) {
        for row in 0..h {
            let syy = sy + row;
            let dyy = dy + row;
            if syy >= src_h || dyy >= self.h {
                break;
            }
            for col in 0..w {
                let sxx = sx + col;
                let dxx = dx + col;
                if sxx >= src_w || dxx >= self.w {
                    break;
                }
                let si = (syy * src_w + sxx) * 4;
                let di = (dyy * self.w + dxx) * 4;
                self.data[di] = rgba[si];
                self.data[di + 1] = rgba[si + 1];
                self.data[di + 2] = rgba[si + 2];
                self.data[di + 3] = 0xFF;
            }
        }
    }
}
