//! Gradient, bucket, picker and constraint values.

use super::{BLACK, RED};
use crate::model::bitmap::{Bitmap, WHITE};
use crate::model::image::{Document, Layer};
use crate::model::rect::Rect;
use crate::model::tools::{
    BrushOptions, Clip, FillArea, FillOptions, Gradient, GradientKind, GradientOptions,
    PickerOptions, Sample, SampleSize, bucket_mask, bucket_mask_in, constrain, mix, pick, sample,
    within_tolerance,
};

fn gradient(kind: GradientKind, start: (f32, f32), end: (f32, f32)) -> Gradient {
    Gradient::new(GradientOptions { kind }, start, end, BLACK, WHITE)
}

#[test]
fn linear_gradient_endpoints_midpoint_and_outside() {
    // Pixel centres 0.5 .. 10.5: pixel 0 is the start, pixel 10 the end.
    let g = gradient(GradientKind::Linear, (0.5, 0.5), (10.5, 0.5));
    assert_eq!(g.at(0, 0), BLACK);
    assert_eq!(g.at(10, 0), WHITE);
    assert_eq!(g.at(5, 0), [128, 128, 128, 255], "midpoint");
    assert_eq!(g.at(-40, 3), BLACK, "before the start");
    assert_eq!(g.at(99, 3), WHITE, "after the end");
    assert_eq!(
        g.at(5, 90),
        [128, 128, 128, 255],
        "constant across the drag"
    );
    // Quarter way: 25 % of the way from black to white.
    let q = gradient(GradientKind::Linear, (0.0, 0.0), (100.0, 0.0));
    assert_eq!(q.at(24, 0)[0], 62, "t = 0.245");
}

#[test]
fn reflected_radial_and_diamond_shapes() {
    let r = gradient(GradientKind::LinearReflected, (20.5, 0.5), (30.5, 0.5));
    assert_eq!(r.at(20, 0), BLACK);
    assert_eq!(r.at(30, 0), WHITE);
    assert_eq!(r.at(10, 0), WHITE, "mirrored on the far side");
    assert_eq!(r.at(25, 0), r.at(15, 0));

    let c = gradient(GradientKind::Radial, (10.5, 10.5), (20.5, 10.5));
    assert_eq!(c.at(10, 10), BLACK);
    assert_eq!(c.at(20, 10), WHITE);
    assert_eq!(c.at(10, 20), WHITE, "all directions");
    assert_eq!(c.at(15, 10), c.at(10, 5));
    assert_eq!(c.at(50, 50), WHITE);

    // Diamond: Manhattan distance, so (5, 5) is as far as (10, 0).
    let d = gradient(GradientKind::Diamond, (10.5, 10.5), (20.5, 10.5));
    assert_eq!(d.at(10, 10), BLACK);
    assert_eq!(d.at(15, 15), d.at(20, 10));
    assert_eq!(d.at(10 + 5, 10 + 5), WHITE, "|dx| + |dy| = the length");
    assert_eq!(d.at(12, 11)[0], 77, "|2| + |1| = 3 of 10 -> t = 0.3");
}

#[test]
fn gradient_interpolates_alpha_and_a_zero_drag_is_the_end_colour() {
    let a = mix([255, 0, 0, 255], [0, 0, 255, 0], 0.5);
    assert_eq!(a, [255, 0, 0, 128], "colour comes from the opaque end");
    assert_eq!(mix([9, 9, 9, 0], [0, 0, 0, 0], 0.5)[3], 0);
    assert_eq!(mix(RED, BLACK, 0.0), RED);
    assert_eq!(mix(RED, BLACK, 1.0), BLACK);
    let none = gradient(GradientKind::Linear, (5.0, 5.0), (5.0, 5.0));
    assert_eq!(none.at(0, 0), WHITE);
}

#[test]
fn gradient_paint_composites_and_respects_the_clip() {
    let g = Gradient::new(
        GradientOptions::default(),
        (0.5, 0.5),
        (9.5, 0.5),
        [255, 0, 0, 255],
        [255, 0, 0, 0],
    );
    let mut layer = Bitmap::white(10, 4);
    let full = layer.rect();
    let changed = g.paint(&mut layer, full, None);
    assert_eq!(changed, Rect::new(0, 0, 10, 4));
    assert_eq!(layer.get(0, 2), Some(RED));
    assert_eq!(
        layer.get(9, 2),
        Some(WHITE),
        "transparent end shows the layer"
    );
    let mid = layer.get(5, 2).unwrap();
    assert!(mid[1] > 60 && mid[1] < 200 && mid[0] == 255, "{mid:?}");

    let mut layer = Bitmap::white(10, 4);
    let mask = [255u8, 255, 128, 128];
    let clip = Clip::new(Rect::new(0, 1, 2, 2), &mask).unwrap();
    let full = layer.rect();
    g.paint(&mut layer, full, Some(clip));
    assert_eq!(layer.get(0, 0), Some(WHITE), "outside the clip");
    assert_eq!(layer.get(0, 1), Some(RED));
    assert!(layer.get(0, 2).unwrap()[1] > 100, "half-strength row");
    assert_eq!(layer.get(2, 1), Some(WHITE));
}

/// A document of `bottom` and `top`, the top layer active.
fn two_layers(bottom: Bitmap, top: Bitmap) -> Document {
    Document::from_layers(
        vec![Layer::new("bottom", bottom), Layer::new("top", top)],
        1,
    )
    .unwrap()
}

fn striped() -> Bitmap {
    // Columns of grey 0, 40, 80 ... on an 8 x 3 bitmap.
    let mut b = Bitmap::white(8, 3);
    for x in 0..8 {
        for y in 0..3 {
            let v = (x * 40) as u8;
            b.put(x, y, [v, v, v, 255]);
        }
    }
    b
}

#[test]
fn tolerance_edges_are_exact() {
    let seed = [100, 100, 100, 255];
    // 10 % of the largest distance (510) is 51.
    assert!(within_tolerance(seed, [151, 100, 100, 255], 10));
    assert!(!within_tolerance(seed, [152, 100, 100, 255], 10));
    assert!(within_tolerance(seed, seed, 0));
    assert!(!within_tolerance(seed, [101, 100, 100, 255], 0));
    assert!(within_tolerance([0, 0, 0, 0], [255, 255, 255, 255], 100));
    assert!(!within_tolerance([0, 0, 0, 0], [255, 255, 255, 255], 99));
    // Alpha counts as a channel.
    assert!(!within_tolerance(seed, [100, 100, 100, 0], 40));
    assert!(within_tolerance(seed, [100, 100, 100, 0], 50));
}

#[test]
fn bucket_grows_with_tolerance() {
    let image = striped();
    let seed = (2, 1);
    let opts = |tolerance| FillOptions {
        tolerance,
        ..FillOptions::default()
    };
    let width_at = |t| bucket_mask(&image, seed, &opts(t)).map_or(0, |m| m.total() / 255 / 3);
    assert_eq!(width_at(0), 1, "column 2 only");
    // Neighbours differ by 40 per RGB channel: sqrt(3 * 40^2) = 69.3 = 13.6 %.
    assert_eq!(width_at(13), 1);
    assert_eq!(width_at(14), 3);
    assert_eq!(width_at(100), 8);
    let mask = bucket_mask(&image, seed, &opts(14)).unwrap();
    assert_eq!(mask.rect(), Rect::new(1, 0, 3, 3));
    assert_eq!(mask.get(1, 2), 255);
    assert!(bucket_mask(&image, (-1, 0), &opts(0)).is_none());
}

#[test]
fn contiguous_stops_at_walls_and_global_does_not() {
    let mut image = Bitmap::white(9, 5);
    for y in 0..5 {
        image.put(4, y, BLACK);
    }
    let mut opts = FillOptions::default();
    let contiguous = bucket_mask(&image, (0, 0), &opts).unwrap();
    assert_eq!(contiguous.total() / 255, 4 * 5);
    assert_eq!(contiguous.get(5, 0), 0);
    opts.area = FillArea::Global;
    let global = bucket_mask(&image, (0, 0), &opts).unwrap();
    assert_eq!(global.total() / 255, 8 * 5);
    assert_eq!(global.get(5, 0), 255);
    assert_eq!(global.get(4, 0), 0);
    // A diagonal gap is not a connection (4-connected).
    let mut diag = Bitmap::white(4, 4);
    for i in 0..4 {
        diag.put(i, 3 - i, BLACK);
    }
    let m = bucket_mask(&diag, (0, 0), &FillOptions::default()).unwrap();
    assert_eq!(m.total() / 255, 6);
}

#[test]
fn bucket_samples_the_layer_or_the_flattened_image() {
    let mut bottom = Bitmap::white(6, 1);
    let mut top = Bitmap::transparent(6, 1);
    top.put(3, 0, BLACK);
    let layer = FillOptions::default();
    let image = FillOptions {
        sample: Sample::Image,
        ..layer
    };
    let doc = two_layers(bottom.clone(), top.clone());
    // The active (top) layer is transparent except one pixel; the image is
    // white with a black pixel in the same place.
    assert_eq!(
        bucket_mask_in(&doc, (0, 0), &layer).unwrap().total() / 255,
        3
    );
    assert_eq!(
        bucket_mask_in(&doc, (0, 0), &image).unwrap().total() / 255,
        3
    );
    bottom.put(1, 0, RED);
    let doc = two_layers(bottom, top);
    assert_eq!(
        bucket_mask_in(&doc, (0, 0), &layer).unwrap().total() / 255,
        3
    );
    assert_eq!(
        bucket_mask_in(&doc, (0, 0), &image).unwrap().total() / 255,
        1
    );
}

#[test]
fn picker_single_and_averaged_samples() {
    let mut b = Bitmap::new(5, 5, [100, 100, 100, 255]);
    b.put(2, 2, [200, 0, 0, 255]);
    assert_eq!(sample(&b, 2, 2, SampleSize::One), Some([200, 0, 0, 255]));
    // 3x3: eight greys and one red.
    let three = sample(&b, 2, 2, SampleSize::Three).unwrap();
    assert_eq!(three, [111, 89, 89, 255]);
    let five = sample(&b, 2, 2, SampleSize::Five).unwrap();
    assert_eq!(five, [104, 96, 96, 255]);
    assert_eq!(sample(&b, 9, 0, SampleSize::Three), None);
    // At a corner only the pixels inside count.
    assert_eq!(
        sample(&b, 0, 0, SampleSize::Five),
        Some([111, 89, 89, 255]),
        "9 pixels in bounds"
    );
    assert_eq!(
        sample(&b, 0, 0, SampleSize::Three),
        Some([100, 100, 100, 255])
    );
}

#[test]
fn picker_weights_colour_by_alpha() {
    let mut b = Bitmap::transparent(3, 3);
    b.put(0, 0, [255, 0, 0, 255]);
    b.put(1, 0, [0, 0, 255, 85]);
    // Colour: (255*255 red, 85 blue) -> mostly red; alpha: 340 / 9.
    let p = sample(&b, 1, 1, SampleSize::Three).unwrap();
    assert_eq!(p, [191, 0, 64, 38]);
    // A fully transparent window keeps the centre's colour with alpha 0.
    let clear = Bitmap::new(3, 3, [7, 8, 9, 0]);
    assert_eq!(sample(&clear, 1, 1, SampleSize::Five), Some([7, 8, 9, 0]));
}

#[test]
fn picker_reads_the_layer_or_the_image() {
    let doc = two_layers(Bitmap::white(4, 4), Bitmap::transparent(4, 4));
    let layer = PickerOptions::default();
    let image = PickerOptions {
        sample: Sample::Image,
        ..layer
    };
    assert_eq!(pick(&doc, 1, 1, &layer), Some([0, 0, 0, 0]));
    assert_eq!(pick(&doc, 1, 1, &image), Some(WHITE));
    assert_eq!(pick(&doc, 9, 9, &image), None);
}

#[test]
fn shift_constraints() {
    assert_eq!(constrain::square((10, 10), (30, 17)), (30, 30));
    assert_eq!(constrain::square((10, 10), (4, 25)), (-5, 25));
    assert_eq!(constrain::square((10, 10), (10, 10)), (10, 10));
    assert_eq!(
        constrain::square((0, 0), (i32::MAX, i32::MIN)),
        (i32::MAX, i32::MIN)
    );
    let (x, y) = constrain::snap_angle((0.0, 0.0), (100.0, 10.0));
    assert!((x - 100.5).abs() < 0.1 && y.abs() < 1.0e-3, "{x} {y}");
    let (x, y) = constrain::snap_angle((5.0, 5.0), (5.0, 80.0));
    assert!(
        (x - 5.0).abs() < 1.0e-3 && (y - 80.0).abs() < 1.0e-3,
        "straight down"
    );
    assert_eq!(constrain::snap_angle_px((0, 0), (20, 19)), (20, 20));
    assert_eq!(constrain::snap_angle_px((7, 7), (7, 7)), (7, 7));
    // 15 degree steps: 20 degrees snaps to 15, 23 to 30.
    let at = |deg: f32| {
        let r = deg.to_radians();
        constrain::snap_angle((0.0, 0.0), (100.0 * r.cos(), 100.0 * r.sin()))
    };
    let angle = |(x, y): (f32, f32)| y.atan2(x).to_degrees();
    assert!((angle(at(20.0)) - 15.0).abs() < 0.01);
    assert!((angle(at(23.0)) - 30.0).abs() < 0.01);
    assert!((angle(at(-100.0)) + 105.0).abs() < 0.01);
    let _ = BrushOptions::default();
}
