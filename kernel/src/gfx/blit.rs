//! Blitting 4-byte source images onto the framebuffer
//! (docs/performance-plan.md, P3.2).
//!
//! `present` runs this for every damage rectangle with interrupts off, so
//! the inner loops carry no per-pixel bounds checks: the rectangle is clamped
//! once against the source and the framebuffer, each source row is sliced
//! once, and each destination row starts at a pointer computed from clamped
//! coordinates. When the source is already in the framebuffer's byte order
//! (`xuid` composes in it, `display::layout`) a row is one `copy_nonoverlapping`,
//! which the kernel's `memcpy` performs as a `rep movs` string copy.

use bootloader_api::info::PixelFormat;
use core::ptr;

use super::{Framebuffer, Packing};

/// The byte order of a 4-byte source pixel; the fourth byte is ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// `R, G, B, A`: what every client and the kernel's own surfaces draw.
    Rgba,
    /// `B, G, R, A`: the usual 32-bit framebuffer order (QEMU's std VGA).
    Bgra,
}

impl Layout {
    /// The layout a 4-byte-per-pixel framebuffer of `format` stores, if a
    /// source in it can be copied without conversion.
    pub fn native(format: PixelFormat, bytes_per_pixel: usize) -> Option<Layout> {
        match (format, bytes_per_pixel) {
            (PixelFormat::Rgb, 4) => Some(Layout::Rgba),
            (PixelFormat::Bgr, 4) => Some(Layout::Bgra),
            _ => None,
        }
    }

    /// `(r, g, b)` of one source pixel in this layout.
    #[inline(always)]
    fn channels(self, pixel: &[u8; 4]) -> (u8, u8, u8) {
        match self {
            Layout::Rgba => (pixel[0], pixel[1], pixel[2]),
            Layout::Bgra => (pixel[2], pixel[1], pixel[0]),
        }
    }
}

/// A blit rectangle after clamping: `w x h` pixels from source `(sx, sy)`
/// to framebuffer `(dx, dy)`, every pixel inside both.
struct Span {
    sx: usize,
    sy: usize,
    dx: usize,
    dy: usize,
    w: usize,
    h: usize,
}

/// Clamp a blit of `w x h` against a `src_w`-pixel-wide source of `src_h`
/// rows held in `src_len` bytes and an `fb_w x fb_h` framebuffer; `None`
/// when nothing is left.
#[allow(clippy::too_many_arguments)]
fn clamp(
    src_len: usize,
    src_w: usize,
    src_h: usize,
    (sx, sy): (usize, usize),
    (dx, dy): (usize, usize),
    (w, h): (usize, usize),
    (fb_w, fb_h): (usize, usize),
) -> Option<Span> {
    if sx >= src_w || sy >= src_h || dx >= fb_w || dy >= fb_h {
        return None;
    }
    let w = w.min(src_w - sx).min(fb_w - dx);
    // Rows the slice really holds (a short slice ends the blit early).
    let stride = src_w.checked_mul(4)?;
    let row_end = (sx + w) * 4;
    let rows_held = match src_len.checked_sub(row_end) {
        Some(rest) => rest / stride + 1,
        None => 0,
    };
    let h = h
        .min(src_h - sy)
        .min(fb_h - dy)
        .min(rows_held.saturating_sub(sy));
    (w > 0 && h > 0).then_some(Span {
        sx,
        sy,
        dx,
        dy,
        w,
        h,
    })
}

impl Framebuffer {
    /// Blit a sub-rectangle of a 4-byte-per-pixel image in `layout` to the
    /// framebuffer, clamped against the image, the slice and the
    /// framebuffer. `src_w` is the image's row length in pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_region(
        &mut self,
        src: &[u8],
        layout: Layout,
        src_w: usize,
        src_h: usize,
        sx: usize,
        sy: usize,
        dx: usize,
        dy: usize,
        w: usize,
        h: usize,
    ) {
        let fb = (self.width(), self.height());
        let Some(span) = clamp(src.len(), src_w, src_h, (sx, sy), (dx, dy), (w, h), fb) else {
            return;
        };
        let bpp = self.info.bytes_per_pixel;
        let row_bytes = self.info.stride * bpp;
        let packing = self.packing();
        let native = Layout::native(self.info.pixel_format, bpp) == Some(layout);
        for row in 0..span.h {
            let start = ((span.sy + row) * src_w + span.sx) * 4;
            // One bounds check per row: `clamp` proved the row is inside.
            let source = &src[start..start + span.w * 4];
            // SAFETY: `span.dy + row < height` and `span.dx + span.w <= width`
            // (clamped), and `sanitize` keeps every in-bounds pixel inside
            // the mapping, so the row's `span.w * bpp` bytes are framebuffer.
            let dest =
                unsafe { (self.base as *mut u8).add((span.dy + row) * row_bytes + span.dx * bpp) };
            if native {
                // SAFETY: `dest` has `span.w * 4` writable bytes (above, with
                // `bpp == 4` for a native layout); `source` is that long and
                // is user or kernel RAM, never the framebuffer.
                unsafe { ptr::copy_nonoverlapping(source.as_ptr(), dest, source.len()) };
            } else {
                // SAFETY: as above, `dest` holds `span.w` whole pixels.
                unsafe { self.convert_row(source, layout, packing, dest) };
            }
        }
    }

    /// Store `source` (whole pixels in `layout`) at `dest`, converting each
    /// pixel to the framebuffer's packing.
    ///
    /// # Safety
    /// `dest` must start `source.len() / 4` whole framebuffer pixels.
    unsafe fn convert_row(&self, source: &[u8], layout: Layout, packing: Packing, dest: *mut u8) {
        let bpp = self.info.bytes_per_pixel;
        let pixels = source.as_chunks::<4>().0;
        for (index, pixel) in pixels.iter().enumerate() {
            let (r, g, b) = layout.channels(pixel);
            // SAFETY: the caller guarantees pixel `index` is inside the row.
            let p = unsafe { dest.add(index * bpp) };
            match packing {
                Packing::Rgb4 | Packing::Bgr4 => {
                    let bytes = if packing == Packing::Bgr4 {
                        [b, g, r, 0xFF]
                    } else {
                        [r, g, b, 0xFF]
                    };
                    // SAFETY: a whole 4-byte pixel at `p`; unaligned is fine.
                    unsafe { p.cast::<u32>().write_unaligned(u32::from_le_bytes(bytes)) };
                }
                Packing::Rgb3 | Packing::Bgr3 => {
                    let (c0, c2) = if packing == Packing::Bgr3 {
                        (b, r)
                    } else {
                        (r, b)
                    };
                    // SAFETY: a whole 3-byte pixel at `p`.
                    unsafe {
                        p.write(c0);
                        p.add(1).write(g);
                        p.add(2).write(c2);
                    }
                }
                Packing::Other => {
                    let encoded = self.encode(r, g, b);
                    // SAFETY: a whole `bpp`-byte pixel at `p`.
                    unsafe { self.store(p, encoded) };
                }
            }
        }
    }
}
