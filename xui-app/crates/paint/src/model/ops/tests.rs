use super::*;
use crate::model::{Bitmap, Pixel, Rect};

/// A 1x1 bitmap holding `pixel`.
fn one(pixel: Pixel) -> Bitmap {
    Bitmap::new(1, 1, pixel)
}

fn adjusted(pixel: Pixel, f: impl Fn(&mut Bitmap, Rect)) -> Pixel {
    let mut bitmap = one(pixel);
    let rect = bitmap.rect();
    f(&mut bitmap, rect);
    bitmap.get(0, 0).unwrap()
}

/// A 3x2 bitmap whose pixels are all different: `[x, y, 0, 255]`.
fn grid() -> Bitmap {
    let mut b = Bitmap::transparent(3, 2);
    for y in 0..2 {
        for x in 0..3 {
            b.put(x, y, [x as u8, y as u8, 0, 255]);
        }
    }
    b
}

fn at(b: &Bitmap, x: i32, y: i32) -> (u8, u8) {
    let p = b.get(x, y).unwrap();
    (p[0], p[1])
}

#[test]
fn flips_and_turns_move_the_corners() {
    let g = grid();
    let h = flipped_h(&g);
    assert_eq!(at(&h, 0, 0), (2, 0));
    assert_eq!(at(&h, 2, 1), (0, 1));
    let v = flipped_v(&g);
    assert_eq!(at(&v, 0, 0), (0, 1));
    assert_eq!(at(&v, 2, 1), (2, 0));
    let r = rotated_180(&g);
    assert_eq!(at(&r, 0, 0), (2, 1));
    assert_eq!(at(&r, 2, 1), (0, 0));
    // Clockwise: the top-left lands top-right, the top-right bottom-right.
    let cw = rotated_cw(&g);
    assert_eq!(cw.size(), (2, 3));
    assert_eq!(at(&cw, 1, 0), (0, 0));
    assert_eq!(at(&cw, 1, 2), (2, 0));
    assert_eq!(at(&cw, 0, 0), (0, 1));
    let ccw = rotated_ccw(&g);
    assert_eq!(ccw.size(), (2, 3));
    assert_eq!(at(&ccw, 0, 2), (0, 0));
    assert_eq!(at(&ccw, 0, 0), (2, 0));
}

#[test]
fn turns_compose() {
    let g = grid();
    assert_eq!(rotated_ccw(&rotated_cw(&g)), g);
    assert_eq!(rotated_cw(&rotated_cw(&g)), rotated_180(&g));
    assert_eq!(flipped_h(&flipped_h(&g)), g);
    assert_eq!(flipped_v(&flipped_h(&g)), rotated_180(&g));
    assert_eq!(rotated_cw(&rotated_cw(&rotated_cw(&rotated_cw(&g)))), g);
}

#[test]
fn anchors_place_the_old_picture() {
    let old = (4, 4);
    let new = (10, 7);
    assert_eq!(Anchor::TopLeft.offset(old, new), (0, 0));
    assert_eq!(Anchor::Top.offset(old, new), (3, 0));
    assert_eq!(Anchor::TopRight.offset(old, new), (6, 0));
    assert_eq!(Anchor::Center.offset(old, new), (3, 1));
    assert_eq!(Anchor::BottomRight.offset(old, new), (6, 3));
    assert_eq!(Anchor::Left.offset(old, new), (0, 1));
    assert_eq!(Anchor::Bottom.offset(old, new), (3, 3));
    assert_eq!(Anchor::ALL.len(), 9);
    // Shrinking crops: the centre of 4 in 2 starts 1 pixel to the left.
    assert_eq!(Anchor::Center.offset((4, 4), (2, 2)), (-1, -1));
    // Grow then shrink by the same amount comes back to the same offset.
    let there = Anchor::Center.offset((5, 5), (8, 8));
    let back = Anchor::Center.offset((8, 8), (5, 5));
    assert_eq!(there.0 + back.0, 0);
}

#[test]
fn placed_pads_and_crops() {
    let g = grid();
    let big = placed(&g, 5, 4, [9, 9, 9, 9], 1, 1);
    assert_eq!(big.get(0, 0), Some([9, 9, 9, 9]));
    assert_eq!(big.get(1, 1), Some([0, 0, 0, 255]));
    assert_eq!(big.get(3, 2), Some([2, 1, 0, 255]));
    let small = placed(&g, 2, 1, [9, 9, 9, 9], -1, 0);
    assert_eq!(small.size(), (2, 1));
    assert_eq!(small.get(0, 0), Some([1, 0, 0, 255]));
}

#[test]
fn nearest_doubles_and_halves_exactly() {
    let g = grid();
    let up = resample(&g, 6, 4, Resample::Nearest);
    assert_eq!(at(&up, 0, 0), (0, 0));
    assert_eq!(at(&up, 1, 1), (0, 0));
    assert_eq!(at(&up, 2, 2), (1, 1));
    assert_eq!(at(&up, 5, 3), (2, 1));
    let down = resample(&up, 3, 2, Resample::Nearest);
    assert_eq!(down, g);
    // The same size is a plain copy for both methods.
    assert_eq!(resample(&g, 3, 2, Resample::Nearest), g);
    assert_eq!(resample(&g, 3, 2, Resample::Bilinear), g);
}

#[test]
fn bilinear_blends_and_keeps_flat_colours_and_alpha() {
    // Black to white across 2 pixels, stretched to 4: 1/8, 3/8, 5/8, 7/8 ...
    // clamped at the ends (centres 0.5/1.5 map outside [0,1]).
    let mut ramp = Bitmap::transparent(2, 1);
    ramp.put(0, 0, [0, 0, 0, 255]);
    ramp.put(1, 0, [200, 200, 200, 255]);
    let out = resample(&ramp, 4, 1, Resample::Bilinear);
    let row: Vec<u8> = (0..4).map(|x| out.get(x, 0).unwrap()[0]).collect();
    // Source coordinates -0.25, 0.25, 0.75, 1.25 -> 0, 0.25, 0.75, 1 of 200.
    assert_eq!(row, vec![0, 50, 150, 200]);
    assert!(out.pixels().chunks(4).all(|p| p[3] == 255));
    // A transparent neighbour does not darken the red next to it.
    let mut edge = Bitmap::transparent(2, 1);
    edge.put(0, 0, [255, 0, 0, 255]);
    let out = resample(&edge, 4, 1, Resample::Bilinear);
    for x in 0..2 {
        assert_eq!(&out.get(x, 0).unwrap()[..3], &[255, 0, 0], "x {x}");
    }
    assert_eq!(out.get(0, 0).unwrap()[3], 255);
    assert_eq!(out.get(1, 0).unwrap()[3], 191, "3/4 of 255");
    assert_eq!(out.get(3, 0).unwrap(), [0, 0, 0, 0]);
    // A flat picture stays exactly flat at any scale.
    let flat = Bitmap::new(5, 3, [12, 34, 56, 77]);
    let big = resample(&flat, 17, 9, Resample::Bilinear);
    assert!(big.pixels().chunks(4).all(|p| p == [12, 34, 56, 77]));
}

#[test]
fn box_fit_makes_a_thumbnail() {
    let wide = Bitmap::new(512, 256, [10, 20, 30, 255]);
    let t = box_fit(&wide, 256);
    assert_eq!(t.size(), (256, 128));
    assert_eq!(t.get(10, 10), Some([10, 20, 30, 255]));
    let tall = Bitmap::new(100, 1000, [1, 1, 1, 255]);
    assert_eq!(box_fit(&tall, 256).size(), (25, 256));
    let thin = Bitmap::new(1, 5000, [1, 1, 1, 255]);
    assert_eq!(box_fit(&thin, 256).size(), (1, 256));
    let small = Bitmap::new(20, 10, [1, 1, 1, 255]);
    assert_eq!(box_fit(&small, 256), small);
    // Averages: a black and a white column become grey.
    let mut stripes = Bitmap::new(2, 1, [0, 0, 0, 255]);
    stripes.put(1, 0, [255, 255, 255, 255]);
    let one = box_fit(&stripes, 1);
    assert_eq!(one.size(), (1, 1));
    assert_eq!(one.get(0, 0), Some([128, 128, 128, 255]));
}

#[test]
fn invert_keeps_alpha() {
    let f = |p| adjusted(p, invert);
    assert_eq!(f([10, 20, 30, 255]), [245, 235, 225, 255]);
    assert_eq!(f([10, 20, 30, 128]), [245, 235, 225, 128]);
    assert_eq!(f([0, 255, 128, 255]), [255, 0, 127, 255]);
    // Alpha 0: nothing visible changes, and the bytes stay put.
    assert_eq!(f([10, 20, 30, 0]), [10, 20, 30, 0]);
    assert_eq!(f(f([1, 2, 3, 77])), [1, 2, 3, 77]);
}

#[test]
fn black_and_white_uses_luma() {
    let f = |p| adjusted(p, black_and_white);
    assert_eq!(f([255, 0, 0, 255]), [76, 76, 76, 255]);
    assert_eq!(f([0, 255, 0, 255]), [150, 150, 150, 255]);
    assert_eq!(f([0, 0, 255, 255]), [29, 29, 29, 255]);
    assert_eq!(f([255, 255, 255, 255]), [255, 255, 255, 255]);
    assert_eq!(f([0, 0, 0, 255]), [0, 0, 0, 255]);
    assert_eq!(f([90, 90, 90, 255]), [90, 90, 90, 255], "grey is kept");
    assert_eq!(f([255, 0, 0, 100]), [76, 76, 76, 100]);
    assert_eq!(f([255, 0, 0, 0]), [255, 0, 0, 0]);
}

#[test]
fn sepia_tints_the_picture() {
    let f = |p| adjusted(p, sepia);
    assert_eq!(f([100, 100, 100, 255]), [135, 120, 94, 255]);
    assert_eq!(f([0, 0, 0, 255]), [0, 0, 0, 255]);
    assert_eq!(f([255, 255, 255, 255]), [255, 255, 240, 255]);
    assert_eq!(f([100, 100, 100, 40]), [135, 120, 94, 40]);
    assert_eq!(f([100, 100, 100, 0]), [100, 100, 100, 0]);
}

#[test]
fn brightness_adds_and_clamps() {
    let f = |p, b, c| adjusted(p, |bm, r| brightness_contrast(bm, r, b, c));
    assert_eq!(f([100, 150, 200, 255], 20, 0), [120, 170, 220, 255]);
    assert_eq!(f([100, 150, 200, 255], -50, 0), [50, 100, 150, 255]);
    assert_eq!(f([250, 10, 0, 255], 30, 0), [255, 40, 30, 255]);
    assert_eq!(f([250, 10, 0, 255], -30, 0), [220, 0, 0, 255]);
    assert_eq!(f([5, 6, 7, 255], 0, 0), [5, 6, 7, 255]);
    assert_eq!(f([100, 150, 200, 77], 20, 0), [120, 170, 220, 77]);
    assert_eq!(f([100, 150, 200, 0], 20, 0), [100, 150, 200, 0]);
    // The range is clamped to -100..100.
    assert_eq!(
        f([100, 100, 100, 255], 500, 0),
        f([100, 100, 100, 255], 100, 0)
    );
}

#[test]
fn contrast_spreads_around_the_middle() {
    let f = |p, b, c| adjusted(p, |bm, r| brightness_contrast(bm, r, b, c));
    // Contrast 50 doubles the distance from 127.
    assert_eq!(f([200, 200, 200, 255], 0, 50), [255, 255, 255, 255]);
    assert_eq!(f([100, 100, 100, 255], 0, 50), [73, 73, 73, 255]);
    assert_eq!(f([127, 127, 127, 255], 0, 50), [127, 127, 127, 255]);
    // Minus 100 flattens every pixel to the middle grey.
    assert_eq!(f([200, 200, 200, 255], 0, -100), [127, 127, 127, 255]);
    assert_eq!(f([10, 10, 10, 255], 0, -100), [127, 127, 127, 255]);
    // Minus 50 halves the distance: 73 * 50 / 100 truncates to 36, so
    // 200 -> 200 + 36 - 73 = 163.
    assert_eq!(f([200, 200, 200, 255], 0, -50), [163, 163, 163, 255]);
    // Plus 100 thresholds at luma 128 minus the brightness.
    assert_eq!(f([127, 127, 127, 255], 0, 100), [0, 0, 0, 255]);
    assert_eq!(f([128, 128, 128, 255], 0, 100), [255, 255, 255, 255]);
    assert_eq!(f([127, 127, 127, 255], 1, 100), [255, 255, 255, 255]);
    assert_eq!(f([128, 128, 128, 0], 0, 100), [128, 128, 128, 0]);
}

fn hs(p: Pixel, hue: i32, sat: i32, light: i32) -> Pixel {
    adjusted(p, |b, r| hue_saturation(b, r, hue, sat, light))
}

#[test]
fn hue_rotates_the_primaries() {
    let red = [255, 0, 0, 255];
    assert_eq!(hs(red, 120, 100, 0), [0, 255, 0, 255]);
    assert_eq!(hs(red, -120, 100, 0), [0, 0, 255, 255]);
    assert_eq!(hs(red, 180, 100, 0), [0, 255, 255, 255]);
    assert_eq!(hs([0, 255, 0, 255], 120, 100, 0), [0, 0, 255, 255]);
    assert_eq!(hs(red, 0, 100, 0), red, "neutral is exact");
    assert_eq!(hs([255, 0, 0, 90], 120, 100, 0), [0, 255, 0, 90]);
    assert_eq!(hs([255, 0, 0, 0], 120, 100, 0), [255, 0, 0, 0]);
    // Grey has no hue to turn.
    assert_eq!(hs([90, 90, 90, 255], 77, 100, 0), [90, 90, 90, 255]);
}

#[test]
fn saturation_and_lightness() {
    let red = [255, 0, 0, 255];
    assert_eq!(hs(red, 0, 0, 0), [128, 128, 128, 255]);
    assert_eq!(hs(red, 0, 200, 0), red, "already fully saturated");
    // 50% saturation of pure red at l=.5: c=.5, m=.25 -> (191, 64, 64).
    assert_eq!(hs(red, 0, 50, 0), [191, 64, 64, 255]);
    assert_eq!(hs(red, 0, 100, 100), [255, 255, 255, 255]);
    assert_eq!(hs(red, 0, 100, -100), [0, 0, 0, 255]);
    assert_eq!(hs(red, 0, 100, 50), [255, 128, 128, 255]);
    assert_eq!(hs(red, 0, 100, -50), [128, 0, 0, 255]);
    // The ranges are clamped.
    assert_eq!(hs(red, 0, 100, 900), [255, 255, 255, 255]);
    assert_eq!(hs(red, 0, -40, 0), hs(red, 0, 0, 0));
}

#[test]
fn hue_turns_round_trip_within_a_step() {
    for r in (0..=255).step_by(51) {
        for g in (0..=255).step_by(51) {
            for b in (0..=255).step_by(51) {
                let p = [r as u8, g as u8, b as u8, 255];
                let back = hs(hs(p, 180, 100, 0), -180, 100, 0);
                for i in 0..3 {
                    assert!(back[i].abs_diff(p[i]) <= 1, "{p:?} -> {back:?}");
                }
            }
        }
    }
}

#[test]
fn adjustments_stay_inside_the_rect() {
    let mut b = Bitmap::new(4, 4, [10, 10, 10, 255]);
    invert(&mut b, Rect::new(1, 1, 2, 2));
    assert_eq!(b.get(0, 0), Some([10, 10, 10, 255]));
    assert_eq!(b.get(1, 1), Some([245, 245, 245, 255]));
    assert_eq!(b.get(2, 2), Some([245, 245, 245, 255]));
    assert_eq!(b.get(3, 3), Some([10, 10, 10, 255]));
    // A rectangle that is outside, or hostile, changes nothing and cannot panic.
    let before = b.clone();
    invert(&mut b, Rect::new(10, 10, 3, 3));
    invert(&mut b, Rect::from_points((i32::MIN, i32::MIN), (-5, -5)));
    assert_eq!(b, before);
    invert(
        &mut b,
        Rect::from_points((i32::MIN, i32::MIN), (i32::MAX, i32::MAX)),
    );
    assert_eq!(b.get(0, 0), Some([245, 245, 245, 255]));
}

#[test]
fn the_adjustment_enum_matches_the_functions() {
    let cases = [
        Adjustment::Invert,
        Adjustment::BlackAndWhite,
        Adjustment::Sepia,
        Adjustment::BrightnessContrast {
            brightness: 10,
            contrast: 20,
        },
        Adjustment::HueSaturation {
            hue: 30,
            saturation: 120,
            lightness: -10,
        },
    ];
    for adj in cases {
        let mut a = Bitmap::new(2, 2, [90, 140, 200, 255]);
        let mut b = a.clone();
        let rect = a.rect();
        adj.apply(&mut a, rect);
        match adj {
            Adjustment::Invert => invert(&mut b, rect),
            Adjustment::BlackAndWhite => black_and_white(&mut b, rect),
            Adjustment::Sepia => sepia(&mut b, rect),
            Adjustment::BrightnessContrast {
                brightness,
                contrast,
            } => brightness_contrast(&mut b, rect, brightness, contrast),
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => hue_saturation(&mut b, rect, hue, saturation, lightness),
        }
        assert_eq!(a, b, "{}", adj.name());
        assert_ne!(
            a.get(0, 0),
            Some([90, 140, 200, 255]),
            "{} did nothing",
            adj.name()
        );
    }
}
