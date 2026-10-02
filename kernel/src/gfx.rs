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
    /// Wrap the bootloader's framebuffer. The geometry is firmware input, so
    /// it is made self-consistent first: a row is never wider than the stride
    /// and no row lies past `byte_len`, so no in-bounds pixel can address
    /// memory outside the buffer whatever the firmware claimed.
    pub fn new(base: usize, info: FrameBufferInfo) -> Self {
        Framebuffer {
            base,
            info: sanitize(info),
        }
    }

    pub fn width(&self) -> usize {
        self.info.width
    }

    pub fn height(&self) -> usize {
        self.info.height
    }

    /// The `width` x `height` rectangle at `(x, y)` as a framebuffer of its
    /// own, clamped to this one. Every write through the view is clipped to
    /// the rectangle, so a blit at any offset cannot leave it (the logical
    /// screen, `display::logical`, is presented through one).
    pub fn view(&self, x: usize, y: usize, width: usize, height: usize) -> Framebuffer {
        let x = x.min(self.width());
        let y = y.min(self.height());
        let width = width.min(self.width() - x);
        let height = height.min(self.height() - y);
        let mut info = self.info;
        info.width = width;
        info.height = height;
        info.byte_len = match height {
            0 => 0,
            rows => ((rows - 1) * info.stride + width) * info.bytes_per_pixel,
        };
        Framebuffer {
            base: self.base + (y * self.info.stride + x) * self.info.bytes_per_pixel,
            info,
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width(), self.height(), color);
    }

    /// Fill a rectangle (clipped) with one opaque colour, one store per pixel.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        let x_end = x.saturating_add(w).min(self.width());
        let y_end = y.saturating_add(h).min(self.height());
        let pixel = self.encode(color.r, color.g, color.b);
        for row in y.min(y_end)..y_end {
            for col in x.min(x_end)..x_end {
                // Safety: `row < height` and `col < width` were clamped above.
                unsafe { self.store(self.ptr_at(col, row), pixel) };
            }
        }
    }

    /// The pixel's bytes in framebuffer order, packed little-endian. Firmware
    /// mostly hands over 32-bit RGB or BGR; a VBE packed-pixel mode can be
    /// 8-bit grey and a "bitmask" one names its channel positions.
    fn encode(&self, r: u8, g: u8, b: u8) -> u32 {
        match self.info.pixel_format {
            PixelFormat::Bgr => u32::from_le_bytes([b, g, r, 0xFF]),
            PixelFormat::U8 => {
                let luma = (u32::from(r) * 77 + u32::from(g) * 150 + u32::from(b) * 29) >> 8;
                u32::from_le_bytes([luma as u8; 4])
            }
            PixelFormat::Unknown {
                red_position,
                green_position,
                blue_position,
            } => {
                let at = |value: u8, shift: u8| u32::from(value).checked_shl(u32::from(shift));
                at(r, red_position).unwrap_or(0)
                    | at(g, green_position).unwrap_or(0)
                    | at(b, blue_position).unwrap_or(0)
            }
            _ => u32::from_le_bytes([r, g, b, 0xFF]),
        }
    }

    /// Store one encoded pixel: a single 32-bit write at 4 bytes per pixel
    /// (one bus transaction on an uncached or write-combining mapping
    /// instead of four), else `bytes_per_pixel` byte writes (never more, so a
    /// 1- or 2-byte mode is not overrun).
    ///
    /// # Safety
    /// `p` must point at a pixel inside the mapped framebuffer.
    unsafe fn store(&self, p: *mut u8, pixel: u32) {
        let bpp = self.info.bytes_per_pixel;
        if bpp == 4 && (p as usize).is_multiple_of(4) {
            // SAFETY: the caller guarantees a whole 4-byte pixel at `p`, and
            // it was just checked to be aligned for a `u32`.
            unsafe { ptr::write_volatile(p.cast::<u32>(), pixel) };
            return;
        }
        for (i, byte) in pixel.to_le_bytes().into_iter().take(bpp).enumerate() {
            // SAFETY: the caller guarantees a whole `bpp`-byte pixel at `p`.
            unsafe { ptr::write_volatile(p.add(i), byte) };
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
        let pixel = self.encode(color.r, color.g, color.b);
        // Safety: bounds were checked; the framebuffer is a valid mapped region.
        unsafe { self.store(self.ptr_at(x, y), pixel) };
    }

    /// Read a pixel back from the framebuffer.
    pub fn read_pixel(&self, x: usize, y: usize) -> Color {
        if x >= self.width() || y >= self.height() {
            return Color::rgb(0, 0, 0);
        }
        let p = self.ptr_at(x, y);
        let mut bytes = [0u8; 4];
        for (i, byte) in bytes.iter_mut().take(self.info.bytes_per_pixel).enumerate() {
            // Safety: bounds were checked; the pixel has `bytes_per_pixel` bytes.
            *byte = unsafe { ptr::read_volatile(p.add(i)) };
        }
        match self.info.pixel_format {
            PixelFormat::Bgr => Color::rgb(bytes[2], bytes[1], bytes[0]),
            PixelFormat::U8 => Color::rgb(bytes[0], bytes[0], bytes[0]),
            PixelFormat::Unknown {
                red_position,
                green_position,
                blue_position,
            } => {
                let value = u32::from_le_bytes(bytes);
                let at = |shift: u8| value.checked_shr(u32::from(shift)).unwrap_or(0) as u8;
                Color::rgb(at(red_position), at(green_position), at(blue_position))
            }
            _ => Color::rgb(bytes[0], bytes[1], bytes[2]),
        }
    }

    /// Fast path for presenting an opaque RGBA image (4 bytes/pixel, e.g. a
    /// tiny-skia `Pixmap`) over the whole framebuffer. The pixel format is
    /// resolved once and each pixel is one store (see [`Self::store`]), which
    /// is far faster than per-pixel `write_pixel`.
    pub fn blit_rgba(&mut self, rgba: &[u8], width: usize, height: usize) {
        self.blit_rgba_at(rgba, width, height, 0, 0);
    }

    /// Blit an RGBA image so its top-left lands at `(dx, dy)`.
    pub fn blit_rgba_at(&mut self, rgba: &[u8], width: usize, height: usize, dx: usize, dy: usize) {
        self.blit_rgba_region(rgba, width, height, 0, 0, dx, dy, width, height);
    }

    /// Blit a sub-rectangle of an RGBA image to the framebuffer.
    ///
    /// Copies `w * h` pixels from source `(sx, sy)` to destination `(dx, dy)`,
    /// clamping against both the source and the framebuffer bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_rgba_region(
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
        let bpp = self.info.bytes_per_pixel;
        let stride = self.info.stride;
        let base = self.base as *mut u8;
        let (fbw, fbh) = (self.width(), self.height());

        for row in 0..h {
            let syy = sy + row;
            let dyy = dy + row;
            if syy >= src_h || dyy >= fbh {
                break;
            }
            // Safety: syy is within the source image.
            let src = &rgba[(syy * src_w + sx) * 4..];
            // Safety: `dyy < fbh` was just checked, so this row is within the
            // mapped framebuffer.
            let drow = unsafe { base.add(dyy * stride * bpp) };
            for col in 0..w {
                let sxx = sx + col;
                let dxx = dx + col;
                if sxx >= src_w || dxx >= fbw {
                    break;
                }
                let i = col * 4;
                let pixel = self.encode(src[i], src[i + 1], src[i + 2]);
                // Safety: `dxx < fbw` was just checked, so this pixel is
                // within the mapped framebuffer row.
                let p = unsafe { drow.add(dxx * bpp) };
                // Safety: within the framebuffer row; bpp is 3 or 4.
                unsafe { self.store(p, pixel) };
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
        // Row by row, `width` pixels each: a view's rows are not contiguous.
        let span = self.width() * self.info.bytes_per_pixel;
        for y in 0..height - lines {
            // Safety: rows `y` and `y + lines` are both below `height`, and
            // `span` bytes from a row start stay inside that row.
            unsafe {
                let dst = (self.base as *mut u8).add(y * row_bytes);
                let src = dst.add(lines * row_bytes);
                ptr::copy(src, dst, span);
            }
        }
        let width = self.width();
        self.fill_rect(0, height - lines, width, lines, clear);
    }
}

/// Clamp a firmware-reported geometry so every pixel `(x < width, y < height)`
/// lies inside `byte_len`: bytes per pixel 1..=4 (else nothing is drawable),
/// width at most the stride, height at most the rows `byte_len` holds.
pub fn sanitize(mut info: FrameBufferInfo) -> FrameBufferInfo {
    let row_bytes = info.stride.checked_mul(info.bytes_per_pixel).unwrap_or(0);
    if !(1..=4).contains(&info.bytes_per_pixel) || row_bytes == 0 {
        info.width = 0;
        info.height = 0;
        return info;
    }
    info.width = info.width.min(info.stride);
    info.height = info.height.min(info.byte_len / row_bytes);
    info
}
