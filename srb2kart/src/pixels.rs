//! The engine's 8-bit frame to the compositor's pixels.
//!
//! SRB2Kart's software renderer draws a palettized frame (`vid.buffer`,
//! `vid.width` x `vid.height`, `vid.rowbytes` per row) and hands the host a
//! 256-entry RGB palette. A LazyOS surface buffer is top-down RGBA8, so each
//! index becomes `[r, g, b, 255]`. The window can be any size (the user resizes
//! or maximizes it), so the frame is scaled with nearest-neighbour sampling to
//! the largest size that keeps its aspect ratio, centred on black.
//!
//! The frame is large (320x200 up to 1280x800 and more) and drawn every tic,
//! so [`Blitter`] keeps what does not change between frames: the palette as a
//! 256-entry `u32` lookup (rebuilt only when the palette's bytes change, which
//! the game does for fades and flashes), the per-column source index table
//! (rebuilt only when the sizes change) and the black bars (repainted only
//! when the geometry changes). Destination rows that sample the same source
//! row are copied from the previous one instead of mapped again.

/// Where a `src_w` x `src_h` image lands inside a `dst_w` x `dst_h` window:
/// `(x, y, width, height)`, aspect ratio kept, never larger than the window.
pub fn fit(src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> (usize, usize, usize, usize) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return (0, 0, 0, 0);
    }
    // Compare dst_w / src_w with dst_h / src_h without dividing.
    let (width, height) = if dst_w * src_h <= dst_h * src_w {
        (dst_w, (src_h * dst_w / src_w).max(1))
    } else {
        ((src_w * dst_h / src_h).max(1), dst_h)
    };
    ((dst_w - width) / 2, (dst_h - height) / 2, width, height)
}

/// Opaque black as a little-endian RGBA word.
const BLACK: u32 = u32::from_le_bytes([0, 0, 0, 0xff]);

/// What the destination currently shows besides frame content, so the bars
/// are only repainted when it changes: window size and the frame's rectangle.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry {
    dst_w: usize,
    dst_h: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

/// Palette-expanding, scaling blitter with its caches.
pub struct Blitter {
    palette: [u8; 768],
    /// `lut[i]` is palette entry `i` as the little-endian word `[r, g, b, 255]`.
    lut: [u32; 256],
    lut_valid: bool,
    /// `xmap[x]` is the source column of destination column `x` (content only).
    xmap: Vec<u32>,
    xmap_key: (usize, usize),
    painted: Option<Geometry>,
}

impl Default for Blitter {
    fn default() -> Blitter {
        Blitter::new()
    }
}

impl Blitter {
    pub fn new() -> Blitter {
        Blitter {
            palette: [0; 768],
            lut: [BLACK; 256],
            lut_valid: false,
            xmap: Vec::new(),
            xmap_key: (0, 0),
            painted: None,
        }
    }

    /// Make `lut` match `palette`, rebuilding only when its bytes changed.
    fn load_palette(&mut self, palette: &[u8; 768]) {
        if self.lut_valid && self.palette == *palette {
            return;
        }
        self.palette = *palette;
        for (word, rgb) in self.lut.iter_mut().zip(palette.chunks_exact(3)) {
            *word = u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0xff]);
        }
        self.lut_valid = true;
    }

    fn load_xmap(&mut self, src_w: usize, width: usize) {
        if self.xmap_key == (src_w, width) && self.xmap.len() == width {
            return;
        }
        self.xmap = (0..width).map(|x| (x * src_w / width) as u32).collect();
        self.xmap_key = (src_w, width);
    }

    /// Draw the `src_w` x `src_h` indexed frame `src` (rows `pitch` bytes
    /// apart) through `palette` into the RGBA window image `dst` (`dst_w` x
    /// `dst_h`, `dst_w * dst_h * 4` bytes), scaled to [`fit`] and centred on
    /// black. Inconsistent sizes draw nothing: a resize the caller has not
    /// caught up with must not index out of bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn blit(
        &mut self,
        src: &[u8],
        src_w: usize,
        src_h: usize,
        pitch: usize,
        palette: &[u8; 768],
        dst: &mut [u8],
        dst_w: usize,
        dst_h: usize,
    ) {
        let needed = if src_h == 0 {
            0
        } else {
            (src_h - 1) * pitch + src_w
        };
        if src_w == 0 || src_h == 0 || pitch < src_w || src.len() < needed {
            return;
        }
        if dst.len() != dst_w * dst_h * 4 || dst.is_empty() {
            return;
        }
        let (left, top, width, height) = fit(src_w, src_h, dst_w, dst_h);
        let geometry = Geometry {
            dst_w,
            dst_h,
            left,
            top,
            width,
            height,
        };
        if self.painted != Some(geometry) {
            fill_black(dst);
            self.painted = Some(geometry);
        }
        self.load_palette(palette);
        self.load_xmap(src_w, width);

        let row_bytes = dst_w * 4;
        let mut previous: Option<(usize, usize)> = None; // (source row, dest row)
        for dy in 0..height {
            let sy = dy * src_h / height;
            let dst_row = top + dy;
            if let Some((prev_sy, prev_row)) = previous {
                if prev_sy == sy {
                    let from = prev_row * row_bytes + left * 4;
                    dst.copy_within(from..from + width * 4, dst_row * row_bytes + left * 4);
                    previous = Some((sy, dst_row));
                    continue;
                }
            }
            let src_row = &src[sy * pitch..sy * pitch + src_w];
            let out = &mut dst[dst_row * row_bytes + left * 4..][..width * 4];
            for (pixel, &sx) in out.chunks_exact_mut(4).zip(&self.xmap) {
                pixel.copy_from_slice(&self.lut[src_row[sx as usize] as usize].to_le_bytes());
            }
            previous = Some((sy, dst_row));
        }
    }
}

fn fill_black(dst: &mut [u8]) {
    for pixel in dst.chunks_exact_mut(4) {
        pixel.copy_from_slice(&BLACK.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Palette entry `i` is `(i, 255 - i, i / 2)`.
    fn ramp() -> [u8; 768] {
        let mut palette = [0u8; 768];
        for i in 0..256 {
            palette[i * 3] = i as u8;
            palette[i * 3 + 1] = 255 - i as u8;
            palette[i * 3 + 2] = (i / 2) as u8;
        }
        palette
    }

    fn rgba(index: u8) -> [u8; 4] {
        [index, 255 - index, index / 2, 0xff]
    }

    fn pixel(dst: &[u8], dst_w: usize, x: usize, y: usize) -> [u8; 4] {
        dst[(y * dst_w + x) * 4..][..4].try_into().unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn run(src: &[u8], w: usize, h: usize, pitch: usize, dw: usize, dh: usize) -> Vec<u8> {
        let mut dst = vec![7u8; dw * dh * 4];
        Blitter::new().blit(src, w, h, pitch, &ramp(), &mut dst, dw, dh);
        dst
    }

    #[test]
    fn fit_keeps_the_aspect_ratio() {
        assert_eq!(fit(320, 200, 320, 200), (0, 0, 320, 200));
        assert_eq!(fit(320, 200, 640, 400), (0, 0, 640, 400));
        assert_eq!(fit(320, 200, 1000, 200), (340, 0, 320, 200)); // pillarbox
        assert_eq!(fit(320, 200, 320, 300), (0, 50, 320, 200)); // letterbox
        assert_eq!(fit(640, 400, 320, 300), (0, 50, 320, 200)); // shrink
        assert_eq!(fit(320, 200, 0, 10), (0, 0, 0, 0));
        assert_eq!(fit(320, 200, 1, 1), (0, 0, 1, 1));
    }

    #[test]
    fn palette_indices_map_to_opaque_rgba() {
        let src = [0u8, 1, 128, 255];
        let dst = run(&src, 4, 1, 4, 4, 1);
        for (x, &index) in src.iter().enumerate() {
            assert_eq!(pixel(&dst, 4, x, 0), rgba(index));
        }
    }

    #[test]
    fn pitch_wider_than_width_skips_the_padding() {
        // 2x2 frame in 5-byte rows; the 0xee padding must never show.
        let src = [1u8, 2, 0xee, 0xee, 0xee, 3, 4];
        let dst = run(&src, 2, 2, 5, 2, 2);
        assert_eq!(pixel(&dst, 2, 0, 0), rgba(1));
        assert_eq!(pixel(&dst, 2, 1, 0), rgba(2));
        assert_eq!(pixel(&dst, 2, 0, 1), rgba(3));
        assert_eq!(pixel(&dst, 2, 1, 1), rgba(4));
    }

    #[test]
    fn doubling_repeats_each_pixel_and_row() {
        let src = [10u8, 20];
        let dst = run(&src, 2, 1, 2, 4, 2);
        for y in 0..2 {
            assert_eq!(pixel(&dst, 4, 0, y), rgba(10));
            assert_eq!(pixel(&dst, 4, 1, y), rgba(10));
            assert_eq!(pixel(&dst, 4, 2, y), rgba(20));
            assert_eq!(pixel(&dst, 4, 3, y), rgba(20));
        }
    }

    #[test]
    fn non_integer_scale_samples_the_nearest_source() {
        // 3 columns into 5 (a 3x1 frame in a 5x2 window is 5x1): source
        // column = x * 3 / 5 = 0,0,1,1,2.
        let src = [1u8, 2, 3];
        let dst = run(&src, 3, 1, 3, 5, 2);
        let got: Vec<u8> = (0..5).map(|x| pixel(&dst, 5, x, 0)[0]).collect();
        assert_eq!(got, [1, 1, 2, 2, 3]);
        // 3 rows into 5: a 1x3 frame in a 2x5 window is 1x5, source row y * 3 / 5.
        let dst = run(&src, 1, 3, 1, 2, 5);
        let got: Vec<u8> = (0..5).map(|y| pixel(&dst, 2, 0, y)[0]).collect();
        assert_eq!(got, [1, 1, 2, 2, 3]);
    }

    #[test]
    fn bars_are_opaque_black_and_content_is_centred() {
        let src = [200u8];
        let dst = run(&src, 1, 1, 1, 3, 1);
        assert_eq!(pixel(&dst, 3, 0, 0), [0, 0, 0, 0xff]);
        assert_eq!(pixel(&dst, 3, 1, 0), rgba(200));
        assert_eq!(pixel(&dst, 3, 2, 0), [0, 0, 0, 0xff]);
        // Letterbox: 1x1 frame in a 1x3 window.
        let dst = run(&src, 1, 1, 1, 1, 3);
        assert_eq!(pixel(&dst, 1, 0, 0), [0, 0, 0, 0xff]);
        assert_eq!(pixel(&dst, 1, 0, 1), rgba(200));
        assert_eq!(pixel(&dst, 1, 0, 2), [0, 0, 0, 0xff]);
    }

    #[test]
    fn a_palette_change_is_picked_up_and_unchanged_one_is_cached() {
        let mut blitter = Blitter::new();
        let src = [5u8];
        let mut dst = vec![0u8; 4];
        blitter.blit(&src, 1, 1, 1, &ramp(), &mut dst, 1, 1);
        assert_eq!(&dst, &rgba(5));
        let mut flash = ramp();
        flash[15..18].copy_from_slice(&[9, 8, 7]);
        blitter.blit(&src, 1, 1, 1, &flash, &mut dst, 1, 1);
        assert_eq!(&dst, &[9, 8, 7, 0xff]);
        blitter.blit(&src, 1, 1, 1, &ramp(), &mut dst, 1, 1);
        assert_eq!(&dst, &rgba(5));
    }

    #[test]
    fn a_resize_repaints_the_bars() {
        let mut blitter = Blitter::new();
        let src = [9u8];
        let mut wide = vec![0u8; 3 * 4];
        blitter.blit(&src, 1, 1, 1, &ramp(), &mut wide, 3, 1);
        let mut tall = vec![0xaau8; 3 * 4];
        blitter.blit(&src, 1, 1, 1, &ramp(), &mut tall, 1, 3);
        assert_eq!(pixel(&tall, 1, 0, 0), [0, 0, 0, 0xff]);
        assert_eq!(pixel(&tall, 1, 0, 1), rgba(9));
    }

    #[test]
    fn inconsistent_sizes_draw_nothing() {
        let mut dst = vec![9u8; 16];
        let mut blitter = Blitter::new();
        let palette = ramp();
        blitter.blit(&[0; 3], 2, 2, 2, &palette, &mut dst, 2, 2); // too short
        blitter.blit(&[0; 4], 2, 2, 1, &palette, &mut dst, 2, 2); // pitch < width
        blitter.blit(&[0; 4], 2, 2, 2, &palette, &mut dst[..12], 2, 2); // dst size
        blitter.blit(&[], 0, 0, 0, &palette, &mut dst, 2, 2);
        assert!(dst.iter().all(|&b| b == 9));
    }
}
