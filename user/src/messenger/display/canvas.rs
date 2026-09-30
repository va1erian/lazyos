//! The software RGBA8 blitter: [`Rect`], [`Color`], [`Canvas`], and the
//! [`font`] bitmap font used for chrome text and demo labels.

use super::Face;

/// An integer rectangle, used for damage and layout.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Whether the rectangle covers no pixels.
    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// The overlapping rectangle, empty when the two do not intersect.
    pub fn intersect(self, other: Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }

    /// The smallest rectangle covering both.
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// An RGB colour for the software blitter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }
}

/// A software RGBA8 blitter over a mapped shared buffer.
///
/// Every write is clipped to the rectangle being drawn and to the canvas
/// bounds, so a caller can pass an over-large damage rectangle safely.
pub struct Canvas {
    base: *mut u8,
    width: i32,
    height: i32,
}

impl Canvas {
    /// Wrap the address and geometry the `display` syscall reported.
    ///
    /// # Safety
    /// `base` must be an RGBA8 mapping of at least `width * height * 4`
    /// bytes in this task's address space (the value `create_buffer` or
    /// `map_buffer` returned). The caller must also have exclusive access to
    /// that memory for the canvas's whole lifetime: no other Rust reference
    /// to the mapping may be live while the canvas draws.
    pub unsafe fn new(base: u64, width: i32, height: i32) -> Canvas {
        Canvas {
            base: base as *mut u8,
            width,
            height,
        }
    }

    /// The canvas width in pixels.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// The canvas height in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// `rect` clipped to `clip` and the canvas bounds; empty when nothing of
    /// it is visible. Every drawing routine clips once through here, so the
    /// per-row loops below never need a bounds check per pixel.
    fn visible(&self, rect: Rect, clip: Rect) -> Rect {
        let bounds = Rect::new(0, 0, self.width, self.height);
        let r = clip_to(clip_to(rect, clip), bounds);
        if r.is_empty() {
            Rect::default()
        } else {
            r
        }
    }

    /// The bytes of `w` pixels starting at `(x, y)`, or an empty slice when
    /// the span is not entirely inside the canvas. Callers pass spans from
    /// [`Canvas::visible`], so the check is a backstop, not the fast path.
    fn row_mut(&mut self, x: i32, y: i32, w: i32) -> &mut [u8] {
        if w <= 0 || x < 0 || y < 0 || y >= self.height || x.saturating_add(w) > self.width {
            return &mut [];
        }
        let at = (y as usize * self.width as usize + x as usize) * 4;
        // SAFETY: the span lies within row `y` of the `width * height * 4`
        // byte mapping `new` was promised, and `&mut self` guarantees no
        // other reference to it exists for the slice's lifetime.
        unsafe { core::slice::from_raw_parts_mut(self.base.add(at), w as usize * 4) }
    }

    /// Blend `color` over one pixel with coverage `alpha` (0..=255).
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Color, alpha: u8, clip: Rect) {
        if alpha == 0 {
            return;
        }
        if alpha == 0xff {
            return self.fill(Rect::new(x, y, 1, 1), clip, color);
        }
        if self.visible(Rect::new(x, y, 1, 1), clip).is_empty() {
            return;
        }
        let a = alpha as u32;
        let mix = |src: u8, dst: u8| ((src as u32 * a + dst as u32 * (255 - a) + 127) / 255) as u8;
        let px = self.row_mut(x, y, 1);
        for (dst, src) in px.iter_mut().zip([color.r, color.g, color.b]) {
            *dst = mix(src, *dst);
        }
        if let Some(alpha_byte) = px.get_mut(3) {
            *alpha_byte = 0xff;
        }
    }

    /// Draw `text` in a proportional anti-aliased [`Face`]. `y` is the top of
    /// the line box (centre it with [`Face::height`]); returns the final pen x.
    pub fn text_face(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        face: Face,
        color: Color,
        clip: Rect,
    ) -> i32 {
        let baseline = y + face.ascent();
        let mut pen16 = x * 16;
        for ch in text.chars() {
            let (glyph, coverage) = face.glyph(ch);
            let gx = (pen16 + 8) / 16 + glyph.left;
            let gy = baseline + glyph.top;
            for row in 0..glyph.height as i32 {
                for col in 0..glyph.width as i32 {
                    let alpha = coverage[(row * glyph.width as i32 + col) as usize];
                    self.blend_pixel(gx + col, gy + row, color, alpha, clip);
                }
            }
            pen16 += glyph.advance_x16 as i32;
        }
        (pen16 + 8) / 16
    }

    /// Fill `rect` with `color`, clipped to `clip`.
    pub fn fill(&mut self, rect: Rect, clip: Rect, color: Color) {
        let r = self.visible(rect, clip);
        let px = [color.r, color.g, color.b, 0xff];
        for y in r.y..r.y + r.h {
            for dst in self.row_mut(r.x, y, r.w).as_chunks_mut::<4>().0.iter_mut() {
                dst.copy_from_slice(&px);
            }
        }
    }

    /// Invert the colour of every pixel of `rect` (clipped to `clip`): XOR
    /// with white, so the result contrasts with whatever was there. It is its
    /// own inverse, but overlapping inverted rectangles cancel where they
    /// overlap, so callers draw disjoint pieces.
    pub fn invert(&mut self, rect: Rect, clip: Rect) {
        let r = self.visible(rect, clip);
        for y in r.y..r.y + r.h {
            for dst in self.row_mut(r.x, y, r.w).as_chunks_mut::<4>().0.iter_mut() {
                dst[0] ^= 0xff;
                dst[1] ^= 0xff;
                dst[2] ^= 0xff;
                dst[3] = 0xff;
            }
        }
    }

    /// Copy a tightly packed RGBA8 source image into `dst`, clipped to
    /// `clip`. `src_w` is the source row length in pixels; rows and columns
    /// past the source are ignored. The destination is always opaque.
    pub fn blit(&mut self, src: &[u8], src_w: i32, src_h: i32, dst: Rect, clip: Rect) {
        if src_w <= 0 || src_h <= 0 {
            return;
        }
        // Only the part of `dst` the source can cover is drawn.
        let covered = Rect::new(dst.x, dst.y, dst.w.min(src_w), dst.h.min(src_h));
        let r = self.visible(covered, clip);
        for y in r.y..r.y + r.h {
            let start = ((y - dst.y) as usize * src_w as usize + (r.x - dst.x) as usize) * 4;
            // A truncated source buffer ends the row early.
            let avail = src.len().saturating_sub(start) / 4;
            let n = (r.w as usize).min(avail);
            if n == 0 {
                break;
            }
            let row = self.row_mut(r.x, y, n as i32);
            row.copy_from_slice(&src[start..start + n * 4]);
            for px in row.as_chunks_mut::<4>().0.iter_mut() {
                px[3] = 0xff;
            }
        }
    }

    /// Draw `text` with the 5x7 font, uppercasing as needed. `scale` is the
    /// pixel size of one font pixel (1 = 5x7, 2 = 10x14).
    pub fn text(&mut self, x: i32, y: i32, text: &str, color: Color, clip: Rect, scale: i32) {
        let scale = scale.max(1);
        let visible = self.visible(clip, clip);
        let mut pen = x;
        for ch in text.chars() {
            let glyph_box = Rect::new(pen, y, 5 * scale, font::H * scale);
            if !clip_to(visible, glyph_box).is_empty() {
                if let Some(glyph) = font::glyph(ch) {
                    self.glyph(pen, y, glyph, color, clip, scale);
                }
            }
            pen += font::ADVANCE * scale;
        }
    }

    /// Draw one glyph as horizontal runs, one fill per run of set pixels in
    /// a glyph row instead of one per pixel.
    fn glyph(&mut self, x: i32, y: i32, glyph: &[u8; 5], color: Color, clip: Rect, scale: i32) {
        for row in 0..font::H {
            let mut col = 0usize;
            while col < glyph.len() {
                if glyph[col] & (1 << row) == 0 {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < glyph.len() && glyph[col] & (1 << row) != 0 {
                    col += 1;
                }
                let run = Rect::new(
                    x + start as i32 * scale,
                    y + row * scale,
                    (col - start) as i32 * scale,
                    scale,
                );
                self.fill(run, clip, color);
            }
        }
    }

    /// Draw the mouse cursor sprite with its top-left at `(x, y)`.
    ///
    /// A black outline surrounds the white body, so the cursor stays
    /// visible over both bright and dark pixels. The 10x10 sprite (body
    /// plus one pixel of outline on every side) is composed per row from bit
    /// masks and written with a single clip.
    pub fn cursor(&mut self, x: i32, y: i32, clip: Rect) {
        const SIZE: i32 = 10;
        // Saturate so an extreme pointer position cannot overflow; such a
        // sprite lies off the canvas and `visible` returns an empty rect.
        let (ox, oy) = (x.saturating_sub(1), y.saturating_sub(1));
        let r = self.visible(Rect::new(ox, oy, SIZE, SIZE), clip);
        for py in r.y..r.y + r.h {
            let sy = (py - oy) as usize;
            let (outline, body) = cursor_masks(sy);
            let first = (r.x - ox) as usize;
            let row = self.row_mut(r.x, py, r.w);
            for (i, dst) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let bit = 1u16 << (first + i);
                if body & bit != 0 {
                    dst.copy_from_slice(&[240, 240, 240, 0xff]);
                } else if outline & bit != 0 {
                    dst.copy_from_slice(&[0, 0, 0, 0xff]);
                }
            }
        }
    }
}

/// `rect` intersected with `clip`, without overflowing on extreme extents.
fn clip_to(rect: Rect, clip: Rect) -> Rect {
    let x0 = rect.x.max(clip.x);
    let y0 = rect.y.max(clip.y);
    let x1 = rect
        .x
        .saturating_add(rect.w)
        .min(clip.x.saturating_add(clip.w));
    let y1 = rect
        .y
        .saturating_add(rect.h)
        .min(clip.y.saturating_add(clip.h));
    Rect::new(
        x0,
        y0,
        x1.saturating_sub(x0).max(0),
        y1.saturating_sub(y0).max(0),
    )
}

/// The `(outline, body)` column masks of row `sy` (0..10) of the 10x10
/// cursor sprite; bit `c` is sprite column `c`. The body is the 8x8
/// [`font::CURSOR`] shifted one pixel in; the outline is that body dilated
/// by one pixel in every direction.
fn cursor_masks(sy: usize) -> (u16, u16) {
    let body_row = |r: usize| -> u16 {
        // Sprite column c (1..=8) holds CURSOR bit 0x80 >> (c - 1).
        let bits = font::CURSOR[r] as u16;
        (0..8).fold(0, |m, col| {
            if bits & (0x80 >> col) != 0 {
                m | 1 << (col + 1)
            } else {
                m
            }
        })
    };
    let body = if (1..=8).contains(&sy) {
        body_row(sy - 1)
    } else {
        0
    };
    let mut outline = 0u16;
    // Cursor row `r` spreads its outline over sprite rows r..=r+2.
    for r in sy.saturating_sub(2)..=sy.min(7) {
        let b = body_row(r);
        outline |= b | (b << 1) | (b >> 1);
    }
    (outline, body)
}

/// The 5x7 bitmap font used for decorations and demo text.
///
/// Each glyph is five columns; in a column byte, bit `n` is row `n` with
/// row zero at the top. Only the characters a window title or a demo label
/// needs are defined; anything else is skipped.
pub mod font {
    /// Glyph height in pixels.
    pub const H: i32 = 7;
    /// Advance per character (five glyph columns plus one pixel gap).
    pub const ADVANCE: i32 = 6;

    /// The cursor sprite, one bit per pixel (MSB = leftmost).
    pub const CURSOR: [u8; 8] = [0x80, 0xC0, 0xA0, 0x90, 0x88, 0x84, 0xFC, 0xC0];

    /// Look up a glyph, upper-casing lower-case ASCII first.
    pub fn glyph(ch: char) -> Option<&'static [u8; 5]> {
        let ch = ch.to_ascii_uppercase();
        Some(match ch {
            ' ' => &[0x00, 0x00, 0x00, 0x00, 0x00],
            '-' => &[0x00, 0x08, 0x08, 0x08, 0x00],
            '.' => &[0x00, 0x00, 0x40, 0x00, 0x00],
            ':' => &[0x00, 0x00, 0x24, 0x00, 0x00],
            '/' => &[0x40, 0x30, 0x08, 0x06, 0x01],
            '+' => &[0x00, 0x08, 0x1C, 0x08, 0x00],
            '!' => &[0x00, 0x00, 0x5F, 0x00, 0x00],
            '?' => &[0x02, 0x01, 0x51, 0x09, 0x06],
            '0' => &[0x3E, 0x51, 0x49, 0x45, 0x3E],
            '1' => &[0x00, 0x42, 0x7F, 0x40, 0x00],
            '2' => &[0x42, 0x61, 0x51, 0x49, 0x46],
            '3' => &[0x22, 0x41, 0x49, 0x49, 0x36],
            '4' => &[0x18, 0x14, 0x12, 0x7F, 0x10],
            '5' => &[0x27, 0x45, 0x45, 0x45, 0x39],
            '6' => &[0x3C, 0x4A, 0x49, 0x49, 0x30],
            '7' => &[0x01, 0x71, 0x09, 0x05, 0x03],
            '8' => &[0x3E, 0x41, 0x49, 0x41, 0x3E],
            '9' => &[0x0E, 0x49, 0x49, 0x29, 0x1E],
            'A' => &[0x7E, 0x09, 0x09, 0x09, 0x7E],
            'B' => &[0x7F, 0x49, 0x49, 0x49, 0x36],
            'C' => &[0x3E, 0x41, 0x41, 0x41, 0x22],
            'D' => &[0x7F, 0x41, 0x41, 0x41, 0x3E],
            'E' => &[0x7F, 0x49, 0x49, 0x49, 0x41],
            'F' => &[0x7F, 0x09, 0x09, 0x09, 0x01],
            'G' => &[0x3E, 0x41, 0x49, 0x49, 0x7A],
            'H' => &[0x7F, 0x08, 0x08, 0x08, 0x7F],
            'I' => &[0x41, 0x41, 0x7F, 0x41, 0x41],
            'J' => &[0x70, 0x70, 0x70, 0x7F, 0x0F],
            'K' => &[0x7F, 0x08, 0x14, 0x22, 0x41],
            'L' => &[0x7F, 0x40, 0x40, 0x40, 0x40],
            'M' => &[0x7F, 0x02, 0x04, 0x02, 0x7F],
            'N' => &[0x7F, 0x02, 0x04, 0x08, 0x7F],
            'O' => &[0x3E, 0x41, 0x41, 0x41, 0x3E],
            'P' => &[0x7F, 0x09, 0x09, 0x09, 0x06],
            'Q' => &[0x3E, 0x41, 0x51, 0x61, 0x7E],
            'R' => &[0x7F, 0x09, 0x19, 0x29, 0x46],
            'S' => &[0x26, 0x49, 0x49, 0x49, 0x32],
            'T' => &[0x01, 0x01, 0x7F, 0x01, 0x01],
            'U' => &[0x3F, 0x40, 0x40, 0x40, 0x3F],
            'V' => &[0x1F, 0x20, 0x40, 0x20, 0x1F],
            'W' => &[0x7F, 0x20, 0x18, 0x20, 0x7F],
            'X' => &[0x63, 0x14, 0x08, 0x14, 0x63],
            'Y' => &[0x03, 0x04, 0x78, 0x04, 0x03],
            'Z' => &[0x41, 0x61, 0x51, 0x49, 0x43],
            _ => return None,
        })
    }
}
