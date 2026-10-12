#![forbid(unsafe_code)]

//! Compositing on straight (non-premultiplied) RGBA8.
//!
//! One formula serves layer flattening today and, later, tool coverage and
//! selection masks: source-over of a source colour with an extra 8-bit
//! `opacity` (a layer's opacity, or a brush coverage).

use super::bitmap::Pixel;

/// `x / 255`, rounded to nearest, exact for `x <= 255 * 255`.
#[inline]
pub fn div255(x: u32) -> u32 {
    let t = x + 128;
    (t + (t >> 8)) >> 8
}

/// Composites `src`, scaled by `opacity`, over `dst` (straight alpha,
/// source-over) and returns the result.
///
/// Fully transparent sources leave `dst` alone and fully opaque ones replace
/// it, with no rounding error in either case.
#[inline]
pub fn source_over(dst: Pixel, src: Pixel, opacity: u8) -> Pixel {
    let sa = if opacity == 255 {
        u32::from(src[3])
    } else {
        div255(u32::from(src[3]) * u32::from(opacity))
    };
    if sa == 0 {
        return dst;
    }
    if sa == 255 {
        return [src[0], src[1], src[2], 255];
    }
    let da = u32::from(dst[3]);
    // The destination's remaining visibility under the source.
    let behind = div255(da * (255 - sa));
    let out_a = sa + behind;
    // out_c = (sc*sa + dc*behind) / out_a, in straight alpha.
    let mix = |s: u8, d: u8| -> u8 {
        let num = u32::from(s) * sa + u32::from(d) * behind;
        ((num + out_a / 2) / out_a).min(255) as u8
    };
    [
        mix(src[0], dst[0]),
        mix(src[1], dst[1]),
        mix(src[2], dst[2]),
        out_a as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The textbook formula in floating point.
    fn reference(dst: Pixel, src: Pixel, opacity: u8) -> [f64; 4] {
        let sa = f64::from(src[3]) / 255.0 * f64::from(opacity) / 255.0;
        let da = f64::from(dst[3]) / 255.0;
        let oa = sa + da * (1.0 - sa);
        let c = |s: u8, d: u8| {
            if oa == 0.0 {
                0.0
            } else {
                (f64::from(s) * sa + f64::from(d) * da * (1.0 - sa)) / oa
            }
        };
        [
            c(src[0], dst[0]),
            c(src[1], dst[1]),
            c(src[2], dst[2]),
            oa * 255.0,
        ]
    }

    #[test]
    fn matches_the_reference_within_one_step() {
        let values = [0u8, 1, 37, 128, 200, 254, 255];
        for &sa in &values {
            for &da in &values {
                for &op in &values {
                    let src = [250, 10, 90, sa];
                    let dst = [20, 220, 130, da];
                    let got = source_over(dst, src, op);
                    let want = reference(dst, src, op);
                    for i in 0..4 {
                        // Colour is below 8-bit precision when the result is nearly invisible.
                        if i < 3 && want[3] < 4.0 {
                            continue;
                        }
                        let diff = (f64::from(got[i]) - want[i]).abs();
                        assert!(
                            // Quantised alpha makes colour coarser when it is low.
                            diff <= 1.5 + if i < 3 { 128.0 / want[3] } else { 0.0 },
                            "{src:?} over {dst:?} @ {op}: {got:?} vs {want:?} (channel {i})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_exact_cases_are_exact() {
        let dst = [10, 20, 30, 200];
        assert_eq!(source_over(dst, [1, 2, 3, 0], 255), dst, "alpha 0");
        assert_eq!(source_over(dst, [1, 2, 3, 255], 0), dst, "opacity 0");
        assert_eq!(source_over(dst, [1, 2, 3, 255], 255), [1, 2, 3, 255]);
        assert_eq!(
            source_over([0, 0, 0, 0], [9, 8, 7, 100], 255),
            [9, 8, 7, 100],
            "over nothing keeps the colour"
        );
    }

    #[test]
    fn half_red_over_white() {
        let out = source_over([255, 255, 255, 255], [255, 0, 0, 128], 255);
        assert_eq!(out, [255, 127, 127, 255]);
        // The same through layer opacity.
        let out = source_over([255, 255, 255, 255], [255, 0, 0, 255], 128);
        assert_eq!(out, [255, 127, 127, 255]);
    }

    #[test]
    fn div255_is_round_to_nearest() {
        for x in 0..=255u32 * 255 {
            let want = (x as f64 / 255.0).round() as u32;
            assert_eq!(div255(x), want, "{x}");
        }
    }
}
