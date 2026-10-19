//! Scaling the NES picture into the window.
//!
//! The console draws 256x240; a window is usually larger, and a retro picture
//! wants whole pixels, not a resample. [`Scaler`] keeps the nearest-neighbour
//! maps for one destination size, so a frame is a gather copy per pixel and
//! the picture stays crisp. The NES's pixels are 8:7 on NTSC, so a
//! pixel-perfect picture is a little wider than 256:240; [`Aspect::Square`]
//! is there for people who prefer it as it comes.

use xui_core::Rect;

/// The console's picture.
pub const WIDTH: i32 = 256;
/// The console's picture.
pub const HEIGHT: i32 = 240;

/// How big the picture is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    /// The largest whole scale that fits the window.
    #[default]
    Auto,
    /// A fixed whole scale, 1x to 4x (the window may clip it).
    Fixed(u8),
}

/// The pixel shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Aspect {
    /// The NES's own 8:7 pixels (the picture is 292.6 units wide).
    #[default]
    Pixel8x7,
    /// Square pixels (256 units wide).
    Square,
}

/// The picture's size at `scale`, in the window's pixels.
pub fn picture_size(scale: i32, aspect: Aspect) -> (i32, i32) {
    let scale = scale.max(1);
    let width = match aspect {
        Aspect::Pixel8x7 => (WIDTH * 8 * scale) / 7,
        Aspect::Square => WIDTH * scale,
    };
    (width, HEIGHT * scale)
}

/// The largest whole scale whose picture fits `bounds`.
pub fn fit_scale(bounds: (i32, i32), aspect: Aspect) -> i32 {
    let (width, height) = (bounds.0.max(1), bounds.1.max(1));
    let mut scale = 1;
    while scale < 8 {
        let (w, h) = picture_size(scale + 1, aspect);
        if w > width || h > height {
            break;
        }
        scale += 1;
    }
    scale
}

/// Where the picture goes: the scale and the destination rectangle, centred
/// in `bounds` (a window smaller than the picture clips it).
pub fn layout(bounds: Rect, scale: Scale, aspect: Aspect) -> (i32, i32, i32, i32) {
    let chosen = match scale {
        Scale::Auto => fit_scale((bounds.width(), bounds.height()), aspect),
        Scale::Fixed(scale) => i32::from(scale).clamp(1, 8),
    };
    let (w, h) = picture_size(chosen, aspect);
    let x = bounds.left + (bounds.width() - w).max(0) / 2;
    let y = bounds.top + (bounds.height() - h).max(0) / 2;
    (x, y, w, h)
}

/// Nearest-neighbour scaling for one destination size.
#[derive(Debug, Default)]
pub struct Scaler {
    width: i32,
    height: i32,
    /// Source column for each destination column.
    columns: Vec<u32>,
    /// Source row for each destination row.
    rows: Vec<u32>,
}

impl Scaler {
    pub fn new() -> Scaler {
        Scaler::default()
    }

    /// The destination size this scaler is prepared for.
    pub fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    /// Prepares for a `width` x `height` destination; cheap when unchanged.
    pub fn resize(&mut self, width: i32, height: i32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.columns = (0..width)
            .map(|x| (x * WIDTH / width).clamp(0, WIDTH - 1) as u32 * 4)
            .collect();
        self.rows = (0..height)
            .map(|y| (y * HEIGHT / height).clamp(0, HEIGHT - 1) as u32)
            .collect();
    }

    /// Scales the console's 256x240 RGBA frame into `out` (which must be at
    /// least `width * height * 4` bytes for the size last given to
    /// [`Scaler::resize`]).
    ///
    /// The console's own buffer leaves the alpha byte as it found it (the
    /// core only writes R, G and B), and the canvas composites an image with
    /// its alpha, so every destination pixel is written opaque here.
    pub fn scale_into(&mut self, frame: &[u8], out: &mut [u8]) {
        let stride = self.width as usize * 4;
        for (y, row) in self.rows.iter().enumerate() {
            let src = *row as usize * WIDTH as usize * 4;
            let dst = y * stride;
            let out_row = &mut out[dst..dst + stride];
            for (x, column) in self.columns.iter().enumerate() {
                let from = src + *column as usize;
                let to = x * 4;
                out_row[to..to + 3].copy_from_slice(&frame[from..from + 3]);
                out_row[to + 3] = 0xFF;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Vec<u8> {
        let mut frame = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let at = ((y * WIDTH + x) * 4) as usize;
                frame[at] = x as u8;
                frame[at + 1] = y as u8;
                frame[at + 2] = 0x40;
                frame[at + 3] = 0xFF;
            }
        }
        frame
    }

    #[test]
    fn the_aspects_are_the_picture_shapes() {
        assert_eq!(picture_size(1, Aspect::Square), (256, 240));
        assert_eq!(picture_size(2, Aspect::Square), (512, 480));
        // 256 * 8/7 * 2 = 585.1: the pixel-perfect picture is wider.
        assert_eq!(picture_size(2, Aspect::Pixel8x7), (585, 480));
    }

    #[test]
    fn the_scale_fits_the_window() {
        assert_eq!(fit_scale((512, 480), Aspect::Square), 2);
        assert_eq!(fit_scale((600, 480), Aspect::Square), 2);
        assert_eq!(fit_scale((768, 720), Aspect::Square), 3);
        assert_eq!(fit_scale((100, 100), Aspect::Square), 1);
        assert_eq!(fit_scale((585, 480), Aspect::Pixel8x7), 2);
    }

    #[test]
    fn the_picture_is_centred() {
        let (x, y, w, h) = layout(Rect::new(0, 0, 600, 500), Scale::Auto, Aspect::Square);
        assert_eq!((w, h), (512, 480));
        assert_eq!((x, y), (44, 10));
        // A fixed scale ignores the fit.
        let (_, _, w, h) = layout(Rect::new(0, 0, 600, 500), Scale::Fixed(1), Aspect::Square);
        assert_eq!((w, h), (256, 240));
    }

    #[test]
    fn scaling_is_nearest_neighbour() {
        let frame = frame();
        let mut scaler = Scaler::new();
        scaler.resize(512, 480);
        let mut out = vec![0u8; 512 * 480 * 4];
        scaler.scale_into(&frame, &mut out);
        // Destination (0, 0) is source (0, 0); (2, 3) is source (1, 1).
        assert_eq!(&out[0..4], &[0, 0, 0x40, 0xFF]);
        let at = (3 * 512 + 2) * 4;
        assert_eq!(&out[at..at + 4], &[1, 1, 0x40, 0xFF]);
        // Every destination pixel comes from the frame's own grid.
        let at = (479 * 512 + 511) * 4;
        assert_eq!(&out[at..at + 4], &[255, 239, 0x40, 0xFF]);
    }

    #[test]
    fn a_transparent_frame_is_drawn_opaque() {
        // The console's buffer has no alpha; the canvas would composite it
        // away, so the scaler writes opaque pixels.
        let mut frame = frame();
        for pixel in frame.as_chunks_mut::<4>().0 {
            pixel[3] = 0;
        }
        let mut scaler = Scaler::new();
        scaler.resize(256, 240);
        let mut out = vec![0u8; 256 * 240 * 4];
        scaler.scale_into(&frame, &mut out);
        assert!(out.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0xFF));
        assert_eq!(&out[0..3], &[0, 0, 0x40]);
    }

    #[test]
    fn an_odd_width_still_covers_every_column() {
        let frame = frame();
        let mut scaler = Scaler::new();
        scaler.resize(585, 480);
        let mut out = vec![0u8; 585 * 480 * 4];
        scaler.scale_into(&frame, &mut out);
        let mut last = 0;
        for x in 0..585 {
            let at = x * 4;
            assert!(out[at] >= last, "columns never go backwards");
            last = out[at];
        }
        assert_eq!(last, 255);
    }
}
