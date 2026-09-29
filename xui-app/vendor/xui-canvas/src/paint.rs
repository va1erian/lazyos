#![forbid(unsafe_code)]

//! Path, colour, stroke and gradient helpers for the tiny-skia canvas, plus
//! the clip-stack entry the canvas keeps.

use tiny_skia::GradientStop as SkiaStop;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Shader, SpreadMode,
    Stroke as SkiaStroke, StrokeDash, Transform,
};

use xui_core::backend::{Cap, Corner, Dash, GradientStop, Join, Stroke};
use xui_core::geometry::{Point, Rect};

/// One entry on the canvas's clip stack.
pub(crate) enum Clip {
    /// An axis-aligned rectangular clip.
    Rect(Rect),
    /// A rounded rectangular clip with per-corner elliptical radii.
    Rounded(Rect, [Corner; 4]),
}

impl Clip {
    /// The device-space bounds used to cull a shape.
    pub(crate) fn bounds(&self) -> Rect {
        match self {
            Clip::Rect(rect) | Clip::Rounded(rect, _) => *rect,
        }
    }

    /// The clip outline in device space, or `None` for an empty clip.
    pub(crate) fn path(&self) -> Option<Path> {
        match self {
            Clip::Rect(rect) => rect_path(*rect),
            Clip::Rounded(rect, corners) => corners_path(*rect, *corners),
        }
    }
}

/// The path for `rect`, or `None` when it is empty.
pub(crate) fn rect_path(rect: Rect) -> Option<Path> {
    if rect.is_empty() {
        return None;
    }
    Some(PathBuilder::from_rect(sk_rect(rect)))
}

/// A rounded-rectangle path with a possibly different elliptical radius on each
/// corner, ordered top-left, top-right, bottom-right, bottom-left. Every corner
/// radius is clamped to half the shorter side, as CSS does.
pub(crate) fn corners_path(rect: Rect, corners: [Corner; 4]) -> Option<Path> {
    if rect.is_empty() {
        return None;
    }
    let (left, top) = (rect.left as f32, rect.top as f32);
    let (right, bottom) = (rect.right as f32, rect.bottom as f32);
    let max_x = ((right - left) / 2.0).max(0.0);
    let max_y = ((bottom - top) / 2.0).max(0.0);
    let clamp =
        |corner: Corner| Corner::new(corner.x.clamp(0.0, max_x), corner.y.clamp(0.0, max_y));
    let [tl, tr, br, bl] = corners.map(clamp);

    let mut builder = PathBuilder::new();
    builder.move_to(left + tl.x, top);
    builder.line_to(right - tr.x, top);
    corner(&mut builder, (right, top), (right, top + tr.y), tr);
    builder.line_to(right, bottom - br.y);
    corner(&mut builder, (right, bottom), (right - br.x, bottom), br);
    builder.line_to(left + bl.x, bottom);
    corner(&mut builder, (left, bottom), (left, bottom - bl.y), bl);
    builder.line_to(left, top + tl.y);
    corner(&mut builder, (left, top), (left + tl.x, top), tl);
    builder.close();
    builder.finish()
}

/// Adds a quadratic corner from the current point through `control` to `end`,
/// or a straight line when the radius is zero.
fn corner(builder: &mut PathBuilder, control: (f32, f32), end: (f32, f32), radius: Corner) {
    if radius.x > 0.0 && radius.y > 0.0 {
        builder.quad_to(control.0, control.1, end.0, end.1);
    } else {
        builder.line_to(end.0, end.1);
    }
}

/// Converts an integer rectangle to a tiny-skia rectangle.
pub(crate) fn sk_rect(rect: Rect) -> tiny_skia::Rect {
    tiny_skia::Rect::from_ltrb(
        rect.left as f32,
        rect.top as f32,
        rect.right as f32,
        rect.bottom as f32,
    )
    .unwrap_or_else(|| tiny_skia::Rect::from_ltrb(0.0, 0.0, 0.0, 0.0).expect("empty rect"))
}

/// The intersection of two rectangles.
pub(crate) fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

/// Maps a portable stroke onto a tiny-skia stroke, scaling its width (and with
/// it its dash pattern) by `scale`.
pub(crate) fn skia_stroke(stroke: &Stroke, scale: f32) -> SkiaStroke {
    let width = (stroke.width * scale).max(1.0);
    let dash = match stroke.dash {
        Dash::Solid => None,
        Dash::Dashed => StrokeDash::new(vec![width * 3.0, width * 2.0], 0.0),
        Dash::Dotted => StrokeDash::new(vec![width, width * 2.0], 0.0),
    };
    let line_cap = match stroke.cap {
        Cap::Flat => LineCap::Butt,
        Cap::Square => LineCap::Square,
        Cap::Round => LineCap::Round,
    };
    let line_join = match stroke.join {
        Join::Miter => LineJoin::Miter,
        Join::Bevel => LineJoin::Bevel,
        Join::Round => LineJoin::Round,
    };
    SkiaStroke {
        width,
        dash,
        line_cap,
        line_join,
        ..SkiaStroke::default()
    }
}

/// A paint with a solid colour shader.
pub(crate) fn solid_shader(color: tiny_skia::Color) -> Shader<'static> {
    Shader::SolidColor(color)
}

/// A linear-gradient shader from `start` to `end` in device space.
pub(crate) fn linear_shader(
    start: Point,
    end: Point,
    stops: &[GradientStop],
) -> Option<Shader<'static>> {
    tiny_skia::LinearGradient::new(
        skia_point(start),
        skia_point(end),
        skia_stops(stops),
        SpreadMode::Pad,
        Transform::identity(),
    )
}

/// An elliptical radial-gradient shader centred at `center` with radii
/// `radius_x`×`radius_y` in device space.
pub(crate) fn radial_shader(
    center: Point,
    radius_x: f32,
    radius_y: f32,
    stops: &[GradientStop],
) -> Option<Shader<'static>> {
    // tiny-skia only has circular radial gradients; a unit circle scaled by
    // the radii and translated to the centre is the elliptical one.
    let transform =
        Transform::from_scale(radius_x, radius_y).post_translate(center.x as f32, center.y as f32);
    tiny_skia::RadialGradient::new(
        skia_point(Point::new(0, 0)),
        skia_point(Point::new(0, 0)),
        1.0,
        skia_stops(stops),
        SpreadMode::Pad,
        transform,
    )
}

fn skia_stops(stops: &[GradientStop]) -> Vec<SkiaStop> {
    stops
        .iter()
        .map(|stop| SkiaStop::new(stop.position, crate::to_skia_rgba(stop.color)))
        .collect()
}

fn skia_point(point: Point) -> tiny_skia::Point {
    tiny_skia::Point::from_xy(point.x as f32, point.y as f32)
}

/// A paint with `shader`, anti-aliased.
pub(crate) fn paint(shader: Shader<'static>) -> Paint<'static> {
    Paint {
        shader,
        anti_alias: true,
        ..Paint::default()
    }
}

/// The winding fill rule every fill uses.
pub(crate) const FILL_RULE: FillRule = FillRule::Winding;
