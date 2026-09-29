#![forbid(unsafe_code)]

//! `fill_path` / `stroke_path` on the software canvas: real curves, joins and
//! the canvas transform.

use crate::Surface;
use xui_core::backend::{Join, PathPlacement, PathSeg, Rgba, Stroke};
use xui_core::geometry::Rect;
use xui_core::{Canvas, Color};

/// A closed half-disc whose arc is a single cubic: (0,0) → (20,0) bulging down.
const BOWL: &[PathSeg] = &[
    PathSeg::MoveTo(0.0, 0.0),
    PathSeg::CubicTo(0.0, 26.0, 20.0, 26.0, 20.0, 0.0),
    PathSeg::Close,
];

fn render(draw: impl FnOnce(&mut dyn Canvas)) -> crate::RgbaImage {
    let mut surface = Surface::new(40, 40);
    surface.fill(Color::rgb(255, 255, 255));
    surface.with_canvas(Rect::new(0, 0, 40, 40), |canvas| draw(canvas));
    surface.to_image()
}

fn is_ink(image: &crate::RgbaImage, x: u32, y: u32) -> bool {
    image.pixel(x, y).is_some_and(|[r, ..]| r < 128)
}

#[test]
fn a_filled_curve_covers_its_belly_but_not_outside_it() {
    let image = render(|canvas| {
        canvas.fill_path(BOWL, PathPlacement::new(1.0, 10.0, 5.0), Rgba::rgb(0, 0, 0));
    });
    assert!(is_ink(&image, 20, 10), "inside the bowl");
    assert!(!is_ink(&image, 12, 25), "below the curve's corner");
    assert!(!is_ink(&image, 20, 3), "above the path");
}

#[test]
fn a_stroked_path_follows_the_placement_and_the_join() {
    let corner = [
        PathSeg::MoveTo(0.0, 10.0),
        PathSeg::LineTo(10.0, 10.0),
        PathSeg::LineTo(10.0, 0.0),
    ];
    let stroke = Stroke::new(4.0).join(Join::Round);
    let image = render(|canvas| {
        canvas.stroke_path(
            &corner,
            PathPlacement::new(2.0, 5.0, 5.0),
            Rgba::rgb(0, 0, 0),
            &stroke,
        );
    });
    assert!(
        is_ink(&image, 15, 25),
        "on the horizontal run (scaled and offset)"
    );
    assert!(is_ink(&image, 25, 15), "on the vertical run");
    assert!(!is_ink(&image, 15, 15), "inside the corner, off the stroke");
}
