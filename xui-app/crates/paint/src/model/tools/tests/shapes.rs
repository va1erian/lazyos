//! Antialiased shape coverage.

use super::{BLACK, BLUE, RED, count_not};
use crate::model::bitmap::{Bitmap, WHITE};
use crate::model::rect::Rect;
use crate::model::tools::{Arrow, FillMode, ShapeOptions, shapes};

fn opts(width: f32, mode: FillMode, aa: bool) -> ShapeOptions {
    ShapeOptions {
        width,
        mode,
        antialias: aa,
        ..ShapeOptions::default()
    }
}

const AREA: Rect = Rect {
    x0: 0,
    y0: 0,
    x1: 100,
    y1: 100,
};

#[test]
fn an_aliased_rectangle_outline_is_hard_and_hollow() {
    let shape = shapes::rectangle((2, 2), (20, 20), &opts(3.0, FillMode::Outline, false), AREA);
    let ring = shape.outline.expect("outline");
    assert!(shape.fill.is_none());
    for &v in ring.data() {
        assert!(v == 0 || v == 255);
    }
    // 19 x 19 outer box minus the 13 x 13 inside.
    assert_eq!(ring.total() / 255, 19 * 19 - 13 * 13);
    assert_eq!(ring.get(2, 2), 255);
    assert_eq!(ring.get(4, 10), 255);
    assert_eq!(ring.get(5, 10), 0, "inside");
    assert_eq!(ring.get(20, 20), 255, "the far corner pixel is included");
    assert_eq!(ring.get(21, 20), 0);
}

#[test]
fn outline_plus_fill_paints_secondary_inside_without_a_fringe() {
    let shape = shapes::rectangle((10, 10), (40, 30), &opts(4.0, FillMode::Both, true), AREA);
    let mut layer = Bitmap::white(60, 60);
    shape.paint(&mut layer, RED, BLUE, None);
    assert_eq!(layer.get(10, 20), Some(RED));
    assert_eq!(layer.get(13, 20), Some(RED));
    assert_eq!(layer.get(25, 20), Some(BLUE));
    assert_eq!(layer.get(9, 20), Some(WHITE), "nothing outside the box");
    // No pixel mixes blue with white: the fill never reaches the rim.
    for y in 0..60 {
        for x in 0..60 {
            let p = layer.get(x, y).unwrap();
            if p != WHITE && p != RED && p != BLUE {
                assert!(p[2] == 255 || p[1] == 255, "fringe {p:?} at {x},{y}");
            }
        }
    }
}

#[test]
fn fill_only_covers_the_whole_box() {
    let shape = shapes::rectangle((5, 5), (14, 14), &opts(8.0, FillMode::Fill, false), AREA);
    let fill = shape.fill.expect("fill");
    assert!(shape.outline.is_none());
    assert_eq!(fill.total() / 255, 100);
}

#[test]
fn a_wide_outline_swallows_a_small_shape() {
    let shape = shapes::ellipse(
        (10, 10),
        (15, 15),
        &opts(50.0, FillMode::Outline, true),
        AREA,
    );
    let ring = shape.outline.expect("outline");
    assert!(
        ring.total() > 20 * 255,
        "not hollow when the width exceeds the shape"
    );
}

#[test]
fn rounded_corners_are_cut_and_square_ones_are_not() {
    let plain = shapes::rectangle((0, 0), (29, 29), &opts(1.0, FillMode::Fill, true), AREA);
    let rounded_opts = ShapeOptions {
        radius: 10.0,
        ..opts(1.0, FillMode::Fill, true)
    };
    let rounded = shapes::rectangle((0, 0), (29, 29), &rounded_opts, AREA);
    assert_eq!(plain.fill.as_ref().unwrap().get(0, 0), 255);
    let corner = rounded.fill.as_ref().unwrap().get(0, 0);
    assert!(corner < 40, "corner {corner}");
    assert_eq!(rounded.fill.as_ref().unwrap().get(15, 0), 255, "edges stay");
    // A radius larger than the box is clamped to a stadium or circle.
    let huge = ShapeOptions {
        radius: 1000.0,
        ..opts(1.0, FillMode::Fill, true)
    };
    let pill = shapes::rectangle((0, 0), (29, 9), &huge, AREA)
        .fill
        .unwrap();
    assert!(pill.get(0, 0) < 40 && pill.get(15, 5) == 255);
}

#[test]
fn an_antialiased_circle_has_the_right_area_and_is_symmetric() {
    let shape = shapes::ellipse((0, 0), (39, 39), &opts(1.0, FillMode::Fill, true), AREA);
    let fill = shape.fill.unwrap();
    let want = std::f64::consts::PI * 400.0 * 255.0;
    assert!((fill.total() as f64 - want).abs() < want * 0.01);
    let mut soft_pixels = 0;
    for y in 0..40 {
        for x in 0..40 {
            // The scan converter rounds in fixed point: mirrors agree within about a sixteenth of a pixel (tiny-skia samples 4 x 4 subpixels).
            for mirror in [fill.get(39 - x, y), fill.get(x, 39 - y)] {
                assert!(
                    fill.get(x, y).abs_diff(mirror) <= 20,
                    "mirror at {x},{y}: {} vs {mirror}",
                    fill.get(x, y)
                );
            }
            let v = fill.get(x, y);
            soft_pixels += usize::from(v != 0 && v != 255);
        }
    }
    assert!(soft_pixels > 40, "an antialiased rim");

    let hard = shapes::ellipse((0, 0), (39, 39), &opts(1.0, FillMode::Fill, false), AREA);
    assert!(
        hard.fill
            .unwrap()
            .data()
            .iter()
            .all(|&v| v == 0 || v == 255)
    );
}

#[test]
fn a_huge_shape_only_rasterizes_the_area() {
    let shape = shapes::rectangle(
        (-1_000_000, -1_000_000),
        (1_000_000, 1_000_000),
        &opts(3.0, FillMode::Both, true),
        Rect::new(0, 0, 16, 16),
    );
    for mask in [
        shape.fill.unwrap(),
        shape
            .outline
            .unwrap_or_else(|| crate::model::tools::Coverage::new(Rect::EMPTY)),
    ] {
        assert!(mask.rect().intersect(Rect::new(0, 0, 16, 16)) == mask.rect());
    }
    let none = shapes::ellipse(
        (500, 500),
        (600, 600),
        &opts(2.0, FillMode::Both, true),
        AREA,
    );
    assert!(none.fill.is_none() && none.outline.is_none());
}

#[test]
fn a_line_has_the_requested_width() {
    let line = shapes::line(
        (5.5, 10.5),
        (40.5, 10.5),
        &opts(3.0, FillMode::Outline, true),
        AREA,
    )
    .unwrap();
    for x in [12, 20, 30] {
        let column: Vec<u8> = (7..14).map(|y| line.get(x, y)).collect();
        assert_eq!(column, [0, 0, 255, 255, 255, 0, 0], "x={x}");
    }
    // Half a pixel more width fades the edge rows to half coverage.
    let wide = shapes::line(
        (5.5, 10.5),
        (40.5, 10.5),
        &opts(4.0, FillMode::Outline, true),
        AREA,
    )
    .unwrap();
    assert!(
        (120..=136).contains(&wide.get(20, 8)),
        "{}",
        wide.get(20, 8)
    );
    // A diagonal is antialiased, an aliased one is not.
    let diagonal = shapes::line(
        (5.0, 5.0),
        (60.0, 33.0),
        &opts(2.0, FillMode::Outline, true),
        AREA,
    )
    .unwrap();
    assert!(diagonal.data().iter().any(|&v| v != 0 && v != 255));
    let hard = shapes::line(
        (5.0, 5.0),
        (60.0, 33.0),
        &opts(2.0, FillMode::Outline, false),
        AREA,
    )
    .unwrap();
    assert!(hard.data().iter().all(|&v| v == 0 || v == 255));
}

#[test]
fn arrow_heads_widen_the_right_ends() {
    let plain = shapes::line(
        (5.5, 20.5),
        (60.5, 20.5),
        &opts(2.0, FillMode::Outline, true),
        AREA,
    )
    .unwrap();
    let at = |arrow| ShapeOptions {
        arrow,
        ..opts(2.0, FillMode::Outline, true)
    };
    let end = shapes::line((5.5, 20.5), (60.5, 20.5), &at(Arrow::End), AREA).unwrap();
    let start = shapes::line((5.5, 20.5), (60.5, 20.5), &at(Arrow::Start), AREA).unwrap();
    let both = shapes::line((5.5, 20.5), (60.5, 20.5), &at(Arrow::Both), AREA).unwrap();
    // 9 px from each tip the head is 4.5 px to a side.
    assert_eq!(plain.get(51, 24), 0);
    assert!(end.get(51, 24) > 128 && end.get(14, 24) == 0);
    assert!(start.get(14, 24) > 128 && start.get(51, 24) == 0);
    assert!(both.get(14, 24) > 128 && both.get(51, 24) > 128);
    // The tip does not overshoot the end point.
    assert_eq!(end.get(62, 20), 0);
    assert!(end.get(60, 20) > 0);
}

#[test]
fn degenerate_lines_do_not_panic() {
    let o = opts(5.0, FillMode::Outline, true);
    assert!(shapes::line((10.0, 10.0), (10.0, 10.0), &o, AREA).is_some());
    let _ = shapes::line((f32::MAX, f32::NAN), (-1.0e30, 5.0), &o, AREA);
    let _ = shapes::line(
        (0.0, 0.0),
        (50.0, 50.0),
        &opts(0.0, FillMode::Outline, true),
        AREA,
    );
    let _ = shapes::rectangle((3, 3), (3, 3), &opts(9.0, FillMode::Both, true), AREA);
    let _ = shapes::ellipse((i32::MIN, 0), (i32::MAX, 0), &o, AREA);
    let mut layer = Bitmap::white(8, 8);
    let s = shapes::rectangle((0, 0), (7, 7), &opts(1.0, FillMode::Outline, false), AREA);
    s.paint(&mut layer, BLACK, WHITE, None);
    assert_eq!(count_not(&layer, WHITE), 28);
}
