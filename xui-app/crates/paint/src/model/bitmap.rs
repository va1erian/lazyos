#![forbid(unsafe_code)]

//! A plain RGBA8 bitmap: the pixels the model draws into.

use xui_core::image::{Image, ImageError};

use super::rect::Rect;

/// The largest canvas side, in pixels.
pub const MAX_SIDE: u32 = 16384;
/// The smallest canvas side, in pixels.
pub const MIN_SIDE: u32 = 1;

/// One RGBA pixel, row-major and top-down.
pub type Pixel = [u8; 4];

/// Opaque white; the canvas background and the eraser's paint.
pub const WHITE: Pixel = [255, 255, 255, 255];

/// An RGBA8 bitmap with clamped, always-valid dimensions.
#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Bitmap {
    /// A `width` x `height` bitmap filled with `fill`, both sides clamped to
    /// `1..=MAX_SIDE`.
    pub fn new(width: u32, height: u32, fill: Pixel) -> Bitmap {
        let width = width.clamp(MIN_SIDE, MAX_SIDE);
        let height = height.clamp(MIN_SIDE, MAX_SIDE);
        let count = width as usize * height as usize;
        let mut pixels = vec![0u8; count * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&fill);
        }
        Bitmap {
            width,
            height,
            pixels,
        }
    }

    /// A white bitmap.
    pub fn white(width: u32, height: u32) -> Bitmap {
        Bitmap::new(width, height, WHITE)
    }

    /// A fully transparent bitmap.
    pub fn transparent(width: u32, height: u32) -> Bitmap {
        Bitmap::new(width, height, [0, 0, 0, 0])
    }

    /// A white `width` x `height` bitmap with this one's pixels copied to the
    /// top-left, cropped when smaller. Sides are clamped like [`Bitmap::new`].
    pub fn resized(&self, width: u32, height: u32) -> Bitmap {
        self.resized_with(width, height, WHITE)
    }

    /// Like [`Bitmap::resized`], with new area filled with `fill`.
    pub fn resized_with(&self, width: u32, height: u32, fill: Pixel) -> Bitmap {
        let mut out = Bitmap::new(width, height, fill);
        let columns = self.width.min(out.width) as usize * 4;
        let rows = self.height.min(out.height) as usize;
        for row in 0..rows {
            let from = row * self.width as usize * 4;
            let to = row * out.width as usize * 4;
            out.pixels[to..to + columns].copy_from_slice(&self.pixels[from..from + columns]);
        }
        out
    }

    /// The bitmap's own bounds as a [`Rect`] at the origin.
    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// A copy of the part of `rect` inside the bitmap, or `None` when that is
    /// empty.
    pub fn crop(&self, rect: Rect) -> Option<Bitmap> {
        let rect = rect.intersect(self.rect());
        if rect.is_empty() {
            return None;
        }
        let (w, h) = (rect.width() as usize, rect.height() as usize);
        let mut pixels = Vec::with_capacity(w * h * 4);
        for y in rect.y0..rect.y1 {
            let at = self.offset(rect.x0, y);
            pixels.extend_from_slice(&self.pixels[at..at + w * 4]);
        }
        Some(Bitmap {
            width: w as u32,
            height: h as u32,
            pixels,
        })
    }

    /// Copies `src_rect` of `src` (clipped to `src`) to `(x, y)` here, clipped
    /// to this bitmap.
    pub fn blit(&mut self, x: i32, y: i32, src: &Bitmap, src_rect: Rect) {
        let src_rect = src_rect.intersect(src.rect());
        // Where the source rectangle lands, clipped to this bitmap.
        let dst = Rect::new(x, y, src_rect.width(), src_rect.height()).intersect(self.rect());
        if dst.is_empty() {
            return;
        }
        let sx = src_rect.x0 + (dst.x0 - x);
        let sy = src_rect.y0 + (dst.y0 - y);
        let bytes = dst.width() as usize * 4;
        for row in 0..dst.height() as i32 {
            let from = src.offset(sx, sy + row);
            let to = self.offset(dst.x0, dst.y0 + row);
            self.pixels[to..to + bytes].copy_from_slice(&src.pixels[from..from + bytes]);
        }
    }

    /// Swaps the pixels of the rectangle at `(x, y)` the size of `other` with
    /// `other`; the rectangle must lie inside this bitmap, else nothing moves
    /// and `false` is returned.
    pub fn swap_region(&mut self, x: i32, y: i32, other: &mut Bitmap) -> bool {
        let rect = Rect::new(x, y, other.width, other.height);
        if rect.intersect(self.rect()) != rect {
            return false;
        }
        let bytes = other.width as usize * 4;
        for row in 0..other.height as usize {
            let at = self.offset(x, y + row as i32);
            let theirs = row * bytes;
            self.pixels[at..at + bytes].swap_with_slice(&mut other.pixels[theirs..theirs + bytes]);
        }
        true
    }

    /// Fills the pixels `x0..x1` of row `y` (clipped) with `fill`.
    pub fn fill_span(&mut self, y: i32, x0: i32, x1: i32, fill: Pixel) {
        let span = Rect {
            x0,
            y0: y,
            x1,
            y1: y.saturating_add(1),
        }
        .intersect(self.rect());
        if span.is_empty() {
            return;
        }
        let at = self.offset(span.x0, span.y0);
        for pixel in self.pixels[at..at + span.width() as usize * 4].as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&fill);
        }
    }

    /// The byte offset of `(x, y)`, which must be inside.
    fn offset(&self, x: i32, y: i32) -> usize {
        (y as usize * self.width as usize + x as usize) * 4
    }

    /// `count` pixels of row `y` starting at column `x`, as bytes; the span must
    /// lie inside the bitmap.
    pub fn row(&self, y: i32, x: i32, count: usize) -> &[u8] {
        let at = self.offset(x, y);
        &self.pixels[at..at + count * 4]
    }

    /// The mutable RGBA8 pixels.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// The width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The size as `(width, height)`.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The tightly packed RGBA8 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The byte length of the buffer.
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    /// Whether `(x, y)` is inside the bitmap.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    /// The pixel at `(x, y)`, or `None` when out of bounds.
    pub fn get(&self, x: i32, y: i32) -> Option<Pixel> {
        if !self.contains(x, y) {
            return None;
        }
        let at = ((y as u32 * self.width + x as u32) * 4) as usize;
        self.pixels
            .get(at..at + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }

    /// Writes `pixel` at `(x, y)`; an out-of-bounds write is ignored.
    pub fn put(&mut self, x: i32, y: i32, pixel: Pixel) {
        if !self.contains(x, y) {
            return;
        }
        let at = ((y as u32 * self.width + x as u32) * 4) as usize;
        if let Some(slot) = self.pixels.get_mut(at..at + 4) {
            slot.copy_from_slice(&pixel);
        }
    }

    /// Fills every pixel with `fill`.
    pub fn fill(&mut self, fill: Pixel) {
        for pixel in self.pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&fill);
        }
    }

    /// A copy of the pixels as an xui [`Image`].
    pub fn to_image(&self) -> Image {
        Image::from_rgba(self.width, self.height, self.pixels.clone())
            .expect("a Bitmap's dimensions and buffer always agree")
    }

    /// A bitmap from a decoded xui [`Image`], clamped to the canvas limits.
    pub fn from_image(image: &Image) -> Bitmap {
        let width = image.width().clamp(MIN_SIDE, MAX_SIDE);
        let height = image.height().clamp(MIN_SIDE, MAX_SIDE);
        // A decoded image can be larger than the canvas cap; sample it into a
        // clamped bitmap rather than rejecting it.
        if (width, height) == image.size() {
            return Bitmap {
                width,
                height,
                pixels: image.pixels().to_vec(),
            };
        }
        let mut bitmap = Bitmap::new(width, height, WHITE);
        for y in 0..height {
            for x in 0..width {
                // u64 so a decoded image wider than u32::MAX/MAX_SIDE cannot
                // overflow the downscale multiply.
                let sx = (u64::from(x) * u64::from(image.width()) / u64::from(width)) as u32;
                let sy = (u64::from(y) * u64::from(image.height()) / u64::from(height)) as u32;
                if let Some(pixel) = image.pixel(sx, sy) {
                    bitmap.put(x as i32, y as i32, pixel);
                }
            }
        }
        bitmap
    }

    /// Encodes the bitmap as a PNG through xui's [`Image`].
    pub fn encode_png(&self) -> Result<Vec<u8>, ImageError> {
        self.to_image().encode_png()
    }

    /// Decodes PNG (or JPEG) bytes through xui's [`Image`] into a bitmap.
    pub fn decode(bytes: &[u8]) -> Result<Bitmap, ImageError> {
        Ok(Bitmap::from_image(&Image::decode(bytes)?))
    }
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bitmap")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_are_clamped_and_valid() {
        assert_eq!(Bitmap::white(0, 0).size(), (1, 1));
        assert_eq!(Bitmap::white(u32::MAX, 1).size(), (MAX_SIDE, 1));
        assert_eq!(Bitmap::white(1, u32::MAX).size(), (1, MAX_SIDE));
        assert_eq!(Bitmap::white(1, 1).byte_len(), 4);
        assert_eq!(Bitmap::white(3, 2).byte_len(), 3 * 2 * 4);
    }

    #[test]
    fn get_and_put_respect_the_bounds() {
        let mut bitmap = Bitmap::white(2, 2);
        assert_eq!(bitmap.get(0, 0), Some(WHITE));
        assert_eq!(bitmap.get(-1, 0), None);
        assert_eq!(bitmap.get(2, 0), None);
        assert_eq!(bitmap.get(0, i32::MAX), None);
        bitmap.put(-5, -5, [1, 2, 3, 4]);
        bitmap.put(1, 1, [9, 8, 7, 6]);
        assert_eq!(bitmap.get(1, 1), Some([9, 8, 7, 6]));
    }

    #[test]
    fn png_round_trips_through_the_model() {
        let mut bitmap = Bitmap::white(4, 3);
        bitmap.put(2, 1, [10, 20, 30, 255]);
        let bytes = bitmap.encode_png().unwrap();
        assert_eq!(Bitmap::decode(&bytes).unwrap(), bitmap);
        assert!(Bitmap::decode(b"not an image").is_err());
    }

    #[test]
    fn an_oversized_image_is_sampled_not_rejected() {
        let big = Image::from_rgba(
            MAX_SIDE + 40,
            8,
            vec![255; (MAX_SIDE as usize + 40) * 8 * 4],
        )
        .unwrap();
        let bitmap = Bitmap::from_image(&big);
        assert_eq!(bitmap.size(), (MAX_SIDE, 8));
    }

    #[test]
    fn crop_blit_and_swap_agree() {
        let mut bitmap = Bitmap::white(6, 4);
        bitmap.put(2, 1, [1, 2, 3, 4]);
        let rect = Rect::new(1, 1, 3, 2);
        let mut piece = bitmap.crop(rect).unwrap();
        assert_eq!(piece.size(), (3, 2));
        assert_eq!(piece.get(1, 0), Some([1, 2, 3, 4]));
        // Swapping in a different piece hands back the old one.
        let mut other = Bitmap::new(3, 2, [9, 9, 9, 9]);
        assert!(bitmap.swap_region(1, 1, &mut other));
        assert_eq!(other, piece);
        assert_eq!(bitmap.get(2, 1), Some([9, 9, 9, 9]));
        // Blitting the original back restores it.
        bitmap.blit(1, 1, &piece, piece.rect());
        assert_eq!(bitmap.get(2, 1), Some([1, 2, 3, 4]));
        // A region outside the bitmap is refused, not clipped.
        assert!(!bitmap.swap_region(5, 3, &mut piece));
        assert!(bitmap.crop(Rect::new(10, 10, 2, 2)).is_none());
    }

    #[test]
    fn blit_clips_and_spans_fill() {
        let mut bitmap = Bitmap::white(4, 4);
        let src = Bitmap::new(3, 3, [5, 5, 5, 255]);
        bitmap.blit(-1, -1, &src, src.rect());
        assert_eq!(bitmap.get(0, 0), Some([5, 5, 5, 255]));
        assert_eq!(bitmap.get(1, 1), Some([5, 5, 5, 255]));
        assert_eq!(bitmap.get(2, 2), Some(WHITE));
        bitmap.fill_span(3, -4, 100, [1, 1, 1, 1]);
        assert_eq!(bitmap.get(3, 3), Some([1, 1, 1, 1]));
        assert_eq!(bitmap.get(0, 3), Some([1, 1, 1, 1]));
    }
}
