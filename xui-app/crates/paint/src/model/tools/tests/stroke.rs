//! The stroke buffer and the brush.

use super::{BLACK, RED, count_not};
use crate::model::bitmap::{Bitmap, WHITE};
use crate::model::rect::Rect;
use crate::model::tools::{BrushOptions, Clip, Coverage, PaintMode, StrokeBuffer, pixel_center};

fn soft(width: u32, hardness: u8) -> BrushOptions {
    BrushOptions {
        width,
        hardness,
        antialias: true,
    }
}

#[test]
fn overlapping_dabs_do_not_build_up_alpha() {
    let mut layer = Bitmap::white(40, 40);
    let mut stroke = StrokeBuffer::new(&layer);
    let color = [0, 0, 0, 128];
    // The same dab 20 times, and a path that crosses itself.
    for _ in 0..20 {
        stroke.dab(&layer, pixel_center(20, 20), soft(10, 100));
    }
    for (a, b) in [
        ((5, 20), (35, 20)),
        ((20, 5), (20, 35)),
        ((5, 20), (35, 20)),
    ] {
        stroke.segment(
            &layer,
            pixel_center(a.0, a.1),
            pixel_center(b.0, b.1),
            soft(6, 100),
        );
    }
    stroke.composite(&mut layer, color, PaintMode::Paint, None);
    // Crossing point, a dab-only point and a single-pass point all agree.
    let at_cross = layer.get(20, 20).unwrap();
    assert_eq!(at_cross, layer.get(10, 20).unwrap());
    assert_eq!(at_cross, layer.get(20, 10).unwrap());
    assert_eq!(at_cross, [127, 127, 127, 255]);
}

#[test]
fn compositing_each_frame_equals_compositing_once() {
    let mut once = Bitmap::white(120, 80);
    let mut framed = once.clone();
    let color = [200, 30, 30, 160];
    let brush = soft(24, 40);
    let points: Vec<(f32, f32)> = (0..40)
        .map(|i| {
            (
                10.0 + i as f32 * 2.5,
                20.0 + (i as f32 * 0.4).sin() * 15.0 + i as f32,
            )
        })
        .collect();

    let mut a = StrokeBuffer::new(&once);
    for pair in points.windows(2) {
        a.segment(&once, pair[0], pair[1], brush);
    }
    a.composite(&mut once, color, PaintMode::Paint, None);

    let mut b = StrokeBuffer::new(&framed);
    for pair in points.windows(2) {
        b.segment(&framed, pair[0], pair[1], brush);
        b.composite(&mut framed, color, PaintMode::Paint, None);
    }
    assert!(
        once == framed,
        "per-frame compositing must not double-apply"
    );
}

#[test]
fn cancel_restores_the_layer() {
    let mut layer = Bitmap::new(60, 60, [10, 200, 30, 255]);
    let original = layer.clone();
    let mut stroke = StrokeBuffer::new(&layer);
    for i in 0..10 {
        stroke.segment(
            &layer,
            (5.0 + i as f32, 5.0),
            (30.0 + i as f32, 40.0),
            soft(12, 100),
        );
        stroke.composite(&mut layer, RED, PaintMode::Paint, None);
    }
    assert!(layer != original);
    let (rect, before) = stroke.before().expect("drew something");
    assert_eq!(
        before.crop(before.rect()).unwrap(),
        original.crop(rect).unwrap()
    );
    let changed = stroke.cancel(&mut layer);
    assert!(!changed.is_empty());
    assert!(layer == original);
}

#[test]
fn erase_reduces_alpha_and_keeps_colour() {
    let mut layer = Bitmap::new(20, 20, [10, 20, 30, 200]);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.dab(&layer, pixel_center(10, 10), soft(8, 100));
    stroke.composite(&mut layer, [0, 0, 0, 255], PaintMode::Erase, None);
    assert_eq!(layer.get(10, 10), Some([0, 0, 0, 0]), "fully erased");
    assert_eq!(
        layer.get(0, 0),
        Some([10, 20, 30, 200]),
        "outside untouched"
    );

    // A half-strength eraser halves alpha and leaves the colour alone.
    let mut layer = Bitmap::new(20, 20, [10, 20, 30, 200]);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.dab(&layer, pixel_center(10, 10), soft(8, 100));
    stroke.composite(&mut layer, [0, 0, 0, 128], PaintMode::Erase, None);
    assert_eq!(layer.get(10, 10), Some([10, 20, 30, 100]));
}

#[test]
fn the_clip_mask_limits_and_scales_the_paint() {
    let mut layer = Bitmap::white(10, 10);
    let rect = Rect::new(2, 2, 4, 4);
    // Left half of the clip full, right half half-strength.
    let mask: Vec<u8> = (0..16).map(|i| if i % 4 < 2 { 255 } else { 128 }).collect();
    let clip = Clip::new(rect, &mask).unwrap();
    let mut cover = Coverage::new(layer.rect());
    cover.data_mut().fill(255);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.add_coverage(&layer, &cover);
    stroke.composite(&mut layer, BLACK, PaintMode::Paint, Some(clip));
    assert_eq!(layer.get(0, 0), Some(WHITE), "outside the clip");
    assert_eq!(layer.get(6, 6), Some(WHITE), "just past the clip");
    assert_eq!(layer.get(2, 2), Some(BLACK));
    assert_eq!(layer.get(5, 5), Some([127, 127, 127, 255]));
    assert_eq!(count_not(&layer, WHITE), 16);
}

#[test]
fn nothing_is_written_outside_the_layer() {
    let mut layer = Bitmap::white(16, 16);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.segment(&layer, (-1.0e9, -50.0), (1.0e9, 80.0), soft(30, 100));
    stroke.segment(&layer, (f32::MAX, 3.0), (-5.0, f32::MIN), soft(500, 0));
    stroke.dab(&layer, (-100.0, -100.0), soft(10, 100));
    stroke.pixel_line(&layer, (i32::MIN, i32::MIN), (i32::MAX, i32::MAX));
    stroke.composite(&mut layer, BLACK, PaintMode::Paint, None);
    assert!(stroke.dirty().intersect(layer.rect()) == stroke.dirty());
    // A dab entirely outside leaves no mark.
    let mut empty = StrokeBuffer::new(&layer);
    assert!(
        empty
            .dab(&layer, (-100.0, -100.0), soft(10, 100))
            .is_empty()
    );
    assert!(empty.is_empty());
}

#[test]
fn a_hard_brush_is_a_disc_and_a_soft_one_falls_off() {
    let layer = Bitmap::white(64, 64);
    let mut hard = StrokeBuffer::new(&layer);
    hard.dab(&layer, (32.0, 32.0), soft(20, 100));
    // Area of a disc of radius 10, within a pixel's rim of antialiasing.
    let area: u64 = (0..64)
        .flat_map(|y| (0..64).map(move |x| (x, y)))
        .map(|(x, y)| u64::from(hard.coverage_at(x, y)))
        .sum();
    let want = std::f64::consts::PI * 100.0 * 255.0;
    assert!((area as f64 - want).abs() < want * 0.02, "{area} vs {want}");
    assert_eq!(hard.coverage_at(32, 32), 255);
    assert_eq!(hard.coverage_at(32, 20), 0);

    let mut soft_brush = StrokeBuffer::new(&layer);
    soft_brush.dab(&layer, (32.0, 32.0), soft(40, 0));
    let centre = soft_brush.coverage_at(31, 31);
    let mid = soft_brush.coverage_at(31 + 10, 31);
    let rim = soft_brush.coverage_at(31 + 19, 31);
    assert!(centre >= 250, "{centre}");
    assert!(mid > rim && mid < centre, "{centre} {mid} {rim}");
    assert!(rim < 20, "rim {rim}");
    // Hardness 50 keeps the inner half solid.
    let mut half = StrokeBuffer::new(&layer);
    half.dab(&layer, (32.0, 32.0), soft(40, 50));
    assert_eq!(half.coverage_at(31 + 8, 31), 255);
    assert!(half.coverage_at(31 + 15, 31) < 255);
}

#[test]
fn aliased_brushes_are_hard_edged() {
    let layer = Bitmap::white(40, 40);
    let mut stroke = StrokeBuffer::new(&layer);
    let aliased = BrushOptions {
        width: 15,
        hardness: 100,
        antialias: false,
    };
    stroke.segment(&layer, (5.5, 5.5), (30.5, 22.5), aliased);
    let mut soft_aliased = StrokeBuffer::new(&layer);
    soft_aliased.dab(
        &layer,
        (20.0, 20.0),
        BrushOptions {
            hardness: 20,
            ..aliased
        },
    );
    for y in 0..40 {
        for x in 0..40 {
            for v in [stroke.coverage_at(x, y), soft_aliased.coverage_at(x, y)] {
                assert!(v == 0 || v == 255, "{v} at {x},{y}");
            }
        }
    }
}

#[test]
fn width_one_aliased_dab_is_exactly_one_pixel() {
    let mut layer = Bitmap::white(8, 8);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.dab(&layer, pixel_center(3, 4), BrushOptions::PENCIL);
    stroke.composite(&mut layer, BLACK, PaintMode::Paint, None);
    assert_eq!(count_not(&layer, WHITE), 1);
    assert_eq!(layer.get(3, 4), Some(BLACK));
}

#[test]
fn a_capsule_has_no_gaps_on_a_fast_drag() {
    let layer = Bitmap::white(300, 40);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.segment(&layer, (5.5, 20.5), (290.5, 20.5), soft(6, 100));
    for x in 5..290 {
        assert_eq!(stroke.coverage_at(x, 20), 255, "gap at {x}");
    }
}

#[test]
fn pixel_line_matches_bresenham_and_is_exact() {
    let mut layer = Bitmap::white(16, 16);
    let mut stroke = StrokeBuffer::new(&layer);
    stroke.pixel_line(&layer, (0, 0), (10, 10));
    stroke.pixel_line(&layer, (0, 15), (15, 12));
    stroke.composite(&mut layer, RED, PaintMode::Paint, None);
    for i in 0..=10 {
        assert_eq!(layer.get(i, i), Some(RED));
    }
    // 11 diagonal pixels + 16 for the shallow line (one per column).
    assert_eq!(count_not(&layer, WHITE), 11 + 16);
}

#[test]
fn growth_keeps_earlier_work() {
    let mut layer = Bitmap::white(2000, 200);
    let mut stroke = StrokeBuffer::new(&layer);
    let mut at = 10.0;
    while at < 1900.0 {
        stroke.segment(&layer, (at, 100.0), (at + 7.0, 100.0), soft(9, 100));
        stroke.composite(&mut layer, RED, PaintMode::Paint, None);
        at += 7.0;
    }
    for x in [10, 500, 1000, 1890] {
        assert_eq!(layer.get(x, 100), Some(RED), "x={x}");
    }
    assert_eq!(layer.get(1950, 100), Some(WHITE));
}
