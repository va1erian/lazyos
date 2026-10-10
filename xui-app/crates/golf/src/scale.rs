//! The internal resolution adapts to the frame cost so frames stay above
//! 20 per second (`game.rs` draws at the scale this picks).

/// Frame work above this (ms) coarsens the internal resolution.
const SLOW_MS: f32 = 40.0;
/// Fewer frames a second than this coarsens it too (the floor is 20).
const MIN_FPS: u32 = 22;

/// The next scale: coarser when a frame costs too much or too few frames
/// are drawn, finer when there is room. The pixel count grows with
/// (s / (s-1))^2 but the cost grows less (sky, sprites and haze scale
/// sublinearly), so the estimate errs on the safe side.
pub(crate) fn adapt(scale: usize, work_ms: f32, fps: u32) -> usize {
    if (work_ms > SLOW_MS || (fps > 0 && fps < MIN_FPS)) && scale < 6 {
        scale + 1
    } else if scale > 1 && fps >= 30 {
        let ratio = scale as f32 / (scale - 1) as f32;
        if work_ms * ratio * ratio < SLOW_MS * 0.8 {
            scale - 1
        } else {
            scale
        }
    } else {
        scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_adapts_both_ways() {
        assert_eq!(adapt(2, 45.0, 18), 3);
        assert_eq!(adapt(2, 10.0, 15), 3, "too few frames is slow too");
        assert_eq!(adapt(3, 5.0, 40), 2);
        assert_eq!(
            adapt(2, 7.0, 60),
            1,
            "QEMU measured 46+ fps at 1x after 7 ms at 2x"
        );
        assert_eq!(adapt(2, 12.0, 40), 2, "finer would cost about 48 ms");
        assert_eq!(adapt(1, 2.0, 60), 1);
        assert_eq!(adapt(6, 90.0, 5), 6);
    }
}
