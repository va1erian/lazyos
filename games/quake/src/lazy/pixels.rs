//! The engine's finished 8-bit frame, as the record delivers it
//! (`FORMAT_RGBA8`: packed through its palette), to the compositor's
//! pixels.
//!
//! Two displays, `vid.rs`'s division (the page's GPU is the DAC; here the
//! CPU is):
//!
//! - a video mode in the 4:3 box (Classic, and every `vid_native 0`
//!   picture): the mode is drawn on a 320x200-shaped store but WinQuake's
//!   modes were 16:10 monitors' fill, so it is shown stretched to 4:3 — the
//!   largest 4:3 box the window fits, as a 1996 CRT did;
//! - native (`vid_native 1`, the slop preset): the picture is the window's
//!   own box at [`crate::vid`]'s pixel size, its aspect, square pixels:
//!   it fills the window whole.
//!
//! Which of the two a record belongs to arrives in the turn's `State`
//! record (the `STATE_NATIVE` flag and its `pixel_size`); the sink keeps
//! the last one. Everything is nearest-neighbour scaled onto an opaque
//! RGBA window image, black bands where the box does not fill the window.

/// The box's aspect ratio as `vid.rs`'s `DISPLAY_ASPECT`: every video mode
/// is shown at it, whatever the frame's own shape.
pub const DISPLAY_ASPECT: f64 = 4.0 / 3.0;

/// Where a `src_w` x `src_h` frame lands inside a `dst_w` x `dst_h` window,
/// displayed at aspect `ratio` (width over height): `(x, y, width, height)`,
/// never larger than the window. `src_w`/`src_h` are not read: the record's
/// shape reaches the screen stretched whole (the mode in the box).
pub fn fit(dst_w: usize, dst_h: usize, ratio: f64) -> (usize, usize, usize, usize) {
    if dst_w == 0 || dst_h == 0 {
        return (0, 0, 0, 0);
    }
    // The largest rect with `width / height == ratio` that fits: try the
    // window's height, then its width, by integer width/height pairs so a
    // ratio of 4/3 gives whole pixels (`dst_w * 3 / 4`).
    let ratio_w = if ratio > 0.0 && ratio.is_finite() { ratio } else { 1.0 };
    let height = dst_h.min((dst_w as f64 / ratio_w) as usize).max(1);
    let width = ((height as f64 * ratio_w) as usize).min(dst_w).max(1);
    ((dst_w - width) / 2, (dst_h - height) / 2, width, height)
}

/// Draw the `w` x `h` RGBA frame (`pixels`, 4 bytes a pixel) into the window
/// image `dst` (`dst_w` x `dst_h`, `dst_w * dst_h * 4` bytes), stretched to
/// [`fit`] and centred on black. Mismatched lengths draw nothing: a resize
/// the caller has not caught up with must not index out of bounds.
/// Draw the `w` x `h` RGBA frame (`pixels`, 4 bytes a pixel) into the window
/// image `dst` (`dst_w` x `dst_h`, `dst_w * dst_h * 4` bytes), stretched to
/// [`fit`] at aspect `ratio` and centred on black; the sink passes the 4:3
/// box ratio for a mode, or the window's own ratio for a native picture
/// (`vid.rs`'s `pixel_size` division). Mismatched lengths draw nothing: a
/// resize the caller has not caught up with must not index out of bounds.
pub fn blit(
    pixels: &[u8],
    w: usize,
    h: usize,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
    ratio: f64,
) {
    if pixels.len() != w * h * 4 || dst.len() != dst_w * dst_h * 4 || w == 0 || h == 0 {
        return;
    }
    let (left, top, box_w, box_h) = fit(dst_w, dst_h, ratio);
    let black = [0, 0, 0, 255];
    for (y, row) in dst.chunks_exact_mut(dst_w * 4).enumerate() {
        if y < top || y >= top + box_h {
            row.as_chunks_mut::<4>().0.iter_mut().for_each(|pixel| *pixel = black);
            continue;
        }
        let src_row = &pixels[(y - top) * h / box_h * w * 4..][..w * 4];
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let value: [u8; 4] = if x < left || x >= left + box_w {
                black
            } else {
                let sx = (x - left) * w / box_w;
                src_row[sx * 4..sx * 4 + 4].try_into().unwrap()
            };
            *pixel = value.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_makes_4_3_whole_pixels() {
        assert_eq!(fit(960, 720, DISPLAY_ASPECT), (0, 0, 960, 720), "a 4:3 window fills whole");
        assert_eq!(fit(800, 720, DISPLAY_ASPECT), (0, 60, 800, 600), "taller window: 4:3 box centred");
        assert_eq!(fit(1280, 600, DISPLAY_ASPECT), (240, 0, 800, 600), "wider window: pillarbox");
        assert_eq!(fit(320, 300, DISPLAY_ASPECT), (0, 30, 320, 240));
        assert_eq!(fit(0, 10, DISPLAY_ASPECT), (0, 0, 0, 0));
        assert_eq!(fit(1, 1, DISPLAY_ASPECT), (0, 0, 1, 1), "at least one pixel");
    }

    #[test]
    fn a_frame_fills_the_box() {
        // A one-pixel frame in a 3x3 window: the 4:3 box is 2x2 at (0,0)
        // (whenever the ratios give an even split, it sits top-left).
        let red = [255u8, 0, 0, 255];
        let mut dst = vec![9u8; 3 * 3 * 4];
        blit(&red, 1, 1, &mut dst, 3, 3, DISPLAY_ASPECT);
        // The box is red; the rest is opaque black.
        assert_eq!(&dst[0..4], &red, "the box's first pixel");
        assert_eq!(&dst[4 * 4..4 * 4 + 4], &red, "inside the box");
        // The last column (x=2) is outside the box: black.
        assert_eq!(&dst[2 * 4..2 * 4 + 4], &[0, 0, 0, 255]);
        assert_eq!(&dst[8 * 4..8 * 4 + 4], &[0, 0, 0, 255]);
    }

    #[test]
    fn a_frame_stretches_across_the_box() {
        // 2x2 frame into a 4x3 window: the box is 4x3, the frame stretches.
        let frame = [1u8, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        let mut dst = vec![0u8; 4 * 3 * 4];
        blit(&frame, 2, 2, &mut dst, 4, 3, DISPLAY_ASPECT);
        let at = |x: usize, y: usize| dst[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4].to_vec();
        assert_eq!(at(0, 0), at(1, 0), "two window columns, one source column");
        assert_eq!(at(0, 0), [1, 2, 3, 255]);
    }

    #[test]
    fn mismatched_buffers_draw_nothing() {
        let mut dst = vec![7u8; 2 * 2 * 4];
        blit(&[0; 3], 2, 2, &mut dst, 2, 2, DISPLAY_ASPECT);
        blit(&[0; 16], 2, 2, &mut dst[..12], 2, 2, DISPLAY_ASPECT);
        assert!(dst.iter().all(|&b| b == 7));
    }
}
