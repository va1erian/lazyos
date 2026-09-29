//! The software RGBA8 blitter: [`Rect`], [`Color`], [`Canvas`], and the
//! [`font`] bitmap font used for chrome text and demo labels.

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
    /// `map_buffer` returned).
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

    /// Write one pixel if it is inside the canvas and the clip rectangle.
    fn pixel(&mut self, x: i32, y: i32, color: Color, clip: Rect) {
        if x < clip.x
            || y < clip.y
            || x >= clip.x + clip.w
            || y >= clip.y + clip.h
            || x < 0
            || y < 0
            || x >= self.width
            || y >= self.height
        {
            return;
        }
        let at = ((y * self.width + x) * 4) as usize;
        // Safety: bounds were checked against the canvas geometry.
        unsafe {
            self.base.add(at).write(color.r);
            self.base.add(at + 1).write(color.g);
            self.base.add(at + 2).write(color.b);
            self.base.add(at + 3).write(0xff);
        }
    }

    /// Fill `rect` with `color`, clipped to `clip`.
    pub fn fill(&mut self, rect: Rect, clip: Rect, color: Color) {
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                self.pixel(x, y, color, clip);
            }
        }
    }

    /// Copy a tightly packed RGBA8 source image into `dst`, clipped to
    /// `clip`. `src_w` is the source row length in pixels; rows and columns
    /// past the source are ignored.
    pub fn blit(&mut self, src: &[u8], src_w: i32, src_h: i32, dst: Rect, clip: Rect) {
        for row in 0..dst.h {
            if row >= src_h {
                break;
            }
            for col in 0..dst.w {
                if col >= src_w {
                    break;
                }
                let at = ((row * src_w + col) * 4) as usize;
                if at + 3 >= src.len() {
                    break;
                }
                let color = Color::rgb(src[at], src[at + 1], src[at + 2]);
                self.pixel(dst.x + col, dst.y + row, color, clip);
            }
        }
    }

    /// Draw `text` with the 5x7 font, uppercasing as needed. `scale` is the
    /// pixel size of one font pixel (1 = 5x7, 2 = 10x14).
    pub fn text(&mut self, x: i32, y: i32, text: &str, color: Color, clip: Rect, scale: i32) {
        let scale = scale.max(1);
        let mut pen = x;
        for ch in text.chars() {
            if ch == ' ' {
                pen += font::ADVANCE * scale;
                continue;
            }
            if let Some(glyph) = font::glyph(ch) {
                for (col, bits) in glyph.iter().enumerate() {
                    for row in 0..font::H {
                        if bits & (1 << row) != 0 {
                            self.fill(
                                Rect::new(
                                    pen + col as i32 * scale,
                                    y + row * scale,
                                    scale,
                                    scale,
                                ),
                                clip,
                                color,
                            );
                        }
                    }
                }
            }
            pen += font::ADVANCE * scale;
        }
    }

    /// Draw the mouse cursor sprite with its top-left at `(x, y)`.
    ///
    /// A black outline is drawn first, then the white body, so the cursor
    /// stays visible over both bright and dark pixels.
    pub fn cursor(&mut self, x: i32, y: i32, clip: Rect) {
        for row in 0..8i32 {
            for col in 0..8i32 {
                if font::CURSOR[row as usize] & (0x80 >> col) == 0 {
                    continue;
                }
                self.fill(
                    Rect::new(x + col - 1, y + row - 1, 3, 3),
                    clip,
                    Color::rgb(0, 0, 0),
                );
            }
        }
        for row in 0..8i32 {
            for col in 0..8i32 {
                if font::CURSOR[row as usize] & (0x80 >> col) != 0 {
                    self.fill(
                        Rect::new(x + col, y + row, 1, 1),
                        clip,
                        Color::rgb(240, 240, 240),
                    );
                }
            }
        }
    }
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
