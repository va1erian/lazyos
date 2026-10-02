//! The engine's framebuffer to the compositor's pixels.
//!
//! doomgeneric renders `DOOMGENERIC_RESX` x `DOOMGENERIC_RESY` 32-bit words,
//! `0x00RRGGBB`. A LazyOS surface buffer is top-down RGBA8 (`docs/architecture/
//! display.md`), so each word becomes the bytes `[r, g, b, 255]`. The window
//! can be any size (the user resizes or maximizes it), so the frame is scaled
//! with nearest-neighbour sampling to the largest size that keeps its aspect
//! ratio, centred on black.

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

/// One `0x00RRGGBB` word as opaque RGBA8 bytes.
pub fn rgba(pixel: u32) -> [u8; 4] {
    let [b, g, r, _] = pixel.to_le_bytes();
    [r, g, b, 0xff]
}

/// Draw the `src_w` x `src_h` frame `src` into the RGBA window image `dst`
/// (`dst_w` x `dst_h`, `dst_w * dst_h * 4` bytes), scaled to [`fit`] and
/// centred on black. Mismatched lengths draw nothing: a resize the caller has
/// not caught up with must not index out of bounds.
pub fn blit(src: &[u32], src_w: usize, src_h: usize, dst: &mut [u8], dst_w: usize, dst_h: usize) {
    if src.len() != src_w * src_h || dst.len() != dst_w * dst_h * 4 {
        return;
    }
    let (left, top, width, height) = fit(src_w, src_h, dst_w, dst_h);
    let black = [0, 0, 0, 0xff];
    for (y, row) in dst.chunks_exact_mut(dst_w * 4).enumerate() {
        if y < top || y >= top + height {
            row.as_chunks_mut::<4>().0.iter_mut().for_each(|pixel| *pixel = black);
            continue;
        }
        let src_row = &src[(y - top) * src_h / height * src_w..][..src_w];
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let value = if x < left || x >= left + width {
                black
            } else {
                rgba(src_row[(x - left) * src_w / width])
            };
            *pixel = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrgb_becomes_opaque_rgba() {
        assert_eq!(rgba(0x0011_2233), [0x11, 0x22, 0x33, 0xff]);
        assert_eq!(rgba(0xff00_0000), [0, 0, 0, 0xff], "the X byte is ignored");
    }

    #[test]
    fn fit_keeps_the_aspect_ratio() {
        assert_eq!(fit(640, 400, 640, 400), (0, 0, 640, 400));
        assert_eq!(fit(640, 400, 1280, 800), (0, 0, 1280, 800));
        // Wider window: pillarbox.
        assert_eq!(fit(640, 400, 1000, 400), (180, 0, 640, 400));
        // Taller window: letterbox.
        assert_eq!(fit(640, 400, 640, 600), (0, 100, 640, 400));
        // Smaller window: shrink.
        assert_eq!(fit(640, 400, 320, 300), (0, 50, 320, 200));
        assert_eq!(fit(640, 400, 0, 10), (0, 0, 0, 0));
        assert_eq!(fit(640, 400, 1, 1), (0, 0, 1, 1));
    }

    #[test]
    fn same_size_is_a_straight_copy() {
        let src: Vec<u32> = (0..6).map(|i| i * 0x0001_0101).collect();
        let mut dst = vec![0u8; 6 * 4];
        blit(&src, 3, 2, &mut dst, 3, 2);
        let expect: Vec<u8> = src.iter().flat_map(|&p| rgba(p)).collect();
        assert_eq!(dst, expect);
    }

    #[test]
    fn doubling_repeats_each_pixel() {
        let src = [0x00ff_0000u32, 0x0000_ff00];
        let mut dst = vec![0u8; 4 * 2 * 4];
        blit(&src, 2, 1, &mut dst, 4, 2);
        let red = rgba(src[0]);
        let green = rgba(src[1]);
        for row in dst.as_chunks::<16>().0 {
            assert_eq!(&row[0..4], &red);
            assert_eq!(&row[4..8], &red);
            assert_eq!(&row[8..12], &green);
            assert_eq!(&row[12..16], &green);
        }
    }

    #[test]
    fn bars_are_opaque_black() {
        let src = [0x00ff_ffffu32];
        let mut dst = vec![7u8; 3 * 4];
        blit(&src, 1, 1, &mut dst, 3, 1);
        assert_eq!(&dst[0..4], &[0, 0, 0, 0xff]);
        assert_eq!(&dst[4..8], &[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(&dst[8..12], &[0, 0, 0, 0xff]);
    }

    #[test]
    fn mismatched_buffers_draw_nothing() {
        let mut dst = vec![9u8; 16];
        blit(&[0; 3], 2, 2, &mut dst, 2, 2);
        blit(&[0; 4], 2, 2, &mut dst[..12], 2, 2);
        assert!(dst.iter().all(|&b| b == 9));
    }
}
