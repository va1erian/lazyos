#![forbid(unsafe_code)]

//! Antialiased shape coverage: lines (with arrow heads), rectangles, rounded
//! rectangles and ellipses, as outline, fill or both.
//!
//! `tiny-skia` (already in the tree through the software backend, pure Rust, 0.11)
//! rasterizes the paths into 8-bit masks, so the edges are exact analytic
//! coverage rather than a supersampling guess, and "antialias off" is its own
//! hard-edged scan conversion. The outline of a closed shape is the outer
//! shape minus the same shape inset by the width, which stays well defined
//! when the width exceeds the shape (the whole shape is then outline) and
//! needs no stroker.
//!
//! Corners are pixel positions (a drag from pixel A to pixel B covers both),
//! so a rectangle spans `min..=max` on each axis. Every function takes the
//! `area` worth rasterizing (normally the layer), so a shape dragged far
//! outside the picture costs only its visible part.

use tiny_skia::{
    FillRule, LineCap, LineJoin, Mask, Path, PathBuilder, Rect as SkRect, Stroke, Transform,
};

use crate::model::bitmap::{Bitmap, Pixel};
use crate::model::rect::Rect;

use super::coverage::{Clip, Coverage};
use super::options::{Arrow, FillMode, ShapeOptions};
use super::stroke::{PaintMode, StrokeBuffer};

/// Coordinates are clamped to this magnitude before any float arithmetic.
const LIMIT: f32 = 1.0e6;
/// The circle-to-cubic constant.
const KAPPA: f32 = 0.552_284_8;

/// What a closed shape paints: the primary-colour outline and the
/// secondary-colour fill, either of which may be absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapeCoverage {
    /// The inside, to be painted with the secondary colour first.
    pub fill: Option<Coverage>,
    /// The outline, painted with the primary colour on top.
    pub outline: Option<Coverage>,
}

impl ShapeCoverage {
    /// Paints the shape on `layer` (fill, then outline) and returns the
    /// rectangle that changed.
    pub fn paint(
        &self,
        layer: &mut Bitmap,
        primary: Pixel,
        secondary: Pixel,
        clip: Option<Clip<'_>>,
    ) -> Rect {
        let mut changed = Rect::EMPTY;
        for (mask, color) in [(&self.fill, secondary), (&self.outline, primary)] {
            let Some(mask) = mask else { continue };
            let mut buffer = StrokeBuffer::new(layer);
            buffer.add_coverage(layer, mask);
            changed = changed.union(buffer.composite(layer, color, PaintMode::Paint, clip));
        }
        changed
    }
}

/// Rasterizes `path` into a mask over its bounds clipped to `area`.
fn fill_path(path: &Path, area: Rect, antialias: bool) -> Option<Coverage> {
    let b = path.bounds();
    let bounds = Rect {
        x0: b.left().floor() as i32,
        y0: b.top().floor() as i32,
        x1: b.right().ceil() as i32,
        y1: b.bottom().ceil() as i32,
    }
    .inflate(1)
    .intersect(area);
    if bounds.is_empty() {
        return None;
    }
    let mut mask = Mask::new(bounds.width(), bounds.height())?;
    let to_local = Transform::from_translate(-bounds.x0 as f32, -bounds.y0 as f32);
    mask.fill_path(path, FillRule::Winding, antialias, to_local);
    Coverage::from_data(bounds, mask.data().to_vec())
}

fn clamp_point(p: (f32, f32)) -> (f32, f32) {
    (p.0.clamp(-LIMIT, LIMIT), p.1.clamp(-LIMIT, LIMIT))
}

/// A rounded rectangle path (a plain one at radius 0).
fn rounded_rect(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> Option<Path> {
    let r = radius
        .min((right - left) / 2.0)
        .min((bottom - top) / 2.0)
        .max(0.0);
    if r < 0.01 {
        return Some(PathBuilder::from_rect(SkRect::from_ltrb(
            left, top, right, bottom,
        )?));
    }
    let k = r * KAPPA;
    let mut p = PathBuilder::new();
    p.move_to(left + r, top);
    p.line_to(right - r, top);
    p.cubic_to(right - r + k, top, right, top + r - k, right, top + r);
    p.line_to(right, bottom - r);
    p.cubic_to(
        right,
        bottom - r + k,
        right - r + k,
        bottom,
        right - r,
        bottom,
    );
    p.line_to(left + r, bottom);
    p.cubic_to(left + r - k, bottom, left, bottom - r + k, left, bottom - r);
    p.line_to(left, top + r);
    p.cubic_to(left, top + r - k, left + r - k, top, left + r, top);
    p.close();
    p.finish()
}

/// Which closed shape to build.
#[derive(Clone, Copy)]
enum Kind {
    Rectangle,
    Ellipse,
}

/// The outline/fill masks of a closed shape in the box `left..right` by
/// `top..bottom` (continuous coordinates).
fn closed(kind: Kind, bx: [f32; 4], options: &ShapeOptions, area: Rect) -> ShapeCoverage {
    let [left, top, right, bottom] = bx;
    let width = options.width.clamp(0.0, 4096.0);
    let radius = options.radius.max(0.0);
    let aa = options.antialias;
    // `inset` shrinks the box and the corner radius together, as an offset
    // curve of the same shape would.
    let build = |inset: f32| -> Option<Coverage> {
        let (l, t, r, b) = (left + inset, top + inset, right - inset, bottom - inset);
        if r - l < 0.001 || b - t < 0.001 {
            return None;
        }
        let path = match kind {
            Kind::Rectangle => rounded_rect(l, t, r, b, (radius - inset).max(0.0)),
            Kind::Ellipse => PathBuilder::from_oval(SkRect::from_ltrb(l, t, r, b)?),
        }?;
        fill_path(&path, area, aa)
    };
    let mut shape = ShapeCoverage::default();
    if options.mode.has_outline() {
        shape.outline = build(0.0).map(|mut outer| {
            if let Some(inner) = build(width) {
                subtract(&mut outer, &inner);
            }
            outer
        });
    }
    if options.mode.has_fill() {
        // Under an outline the fill stops at the stroke's centre line, so no
        // fill colour fringes out at the outline's antialiased rim.
        let inset = if options.mode == FillMode::Both {
            width / 2.0
        } else {
            0.0
        };
        shape.fill = build(inset).or_else(|| build(0.0));
    }
    shape
}

/// `outer *= (1 - inner)`, where the two overlap.
fn subtract(outer: &mut Coverage, inner: &Coverage) {
    let overlap = outer.rect().intersect(inner.rect());
    if overlap.is_empty() {
        return;
    }
    let rect = outer.rect();
    let stride = rect.width() as usize;
    for y in overlap.y0..overlap.y1 {
        let theirs = inner.row(y, overlap.x0, overlap.x1);
        let at = (y - rect.y0) as usize * stride + (overlap.x0 - rect.x0) as usize;
        for (mine, &cut) in outer.data_mut()[at..at + theirs.len()]
            .iter_mut()
            .zip(theirs)
        {
            *mine = crate::model::blend::div255(u32::from(*mine) * u32::from(255 - cut)) as u8;
        }
    }
}

/// The box through two corner pixels, covering both.
fn corner_box(a: (i32, i32), b: (i32, i32)) -> [f32; 4] {
    let lim = LIMIT as i32;
    let (x0, y0) = (a.0.clamp(-lim, lim), a.1.clamp(-lim, lim));
    let (x1, y1) = (b.0.clamp(-lim, lim), b.1.clamp(-lim, lim));
    [
        x0.min(x1) as f32,
        y0.min(y1) as f32,
        (x0.max(x1) + 1) as f32,
        (y0.max(y1) + 1) as f32,
    ]
}

/// A rectangle, rounded when `options.radius > 0`, through two corner pixels.
pub fn rectangle(
    a: (i32, i32),
    b: (i32, i32),
    options: &ShapeOptions,
    area: Rect,
) -> ShapeCoverage {
    closed(Kind::Rectangle, corner_box(a, b), options, area)
}

/// An ellipse inscribed in the box through two corner pixels.
pub fn ellipse(a: (i32, i32), b: (i32, i32), options: &ShapeOptions, area: Rect) -> ShapeCoverage {
    closed(Kind::Ellipse, corner_box(a, b), options, area)
}

/// A triangle with its tip at `tip`, `length` long, `half` wide each side,
/// pointing along the unit vector `dir`.
fn arrow_head(tip: (f32, f32), dir: (f32, f32), length: f32, half: f32) -> Option<Path> {
    let back = (tip.0 - dir.0 * length, tip.1 - dir.1 * length);
    let side = (-dir.1 * half, dir.0 * half);
    let mut p = PathBuilder::new();
    p.move_to(tip.0, tip.1);
    p.line_to(back.0 + side.0, back.1 + side.1);
    p.line_to(back.0 - side.0, back.1 - side.1);
    p.close();
    p.finish()
}

/// A line of `options.width` between two points (continuous coordinates; pass
/// [`pixel_center`](super::pixel_center) for a pixel), with the arrow heads
/// the options ask for. Always an outline: fill modes do not apply.
pub fn line(
    from: (f32, f32),
    to: (f32, f32),
    options: &ShapeOptions,
    area: Rect,
) -> Option<Coverage> {
    let (from, to) = (clamp_point(from), clamp_point(to));
    let width = options.width.clamp(0.0, 4096.0).max(0.01);
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = (dx * dx + dy * dy).sqrt();
    let dir = if length > 0.0 {
        (dx / length, dy / length)
    } else {
        (1.0, 0.0)
    };
    let head = (width * 3.0).max(10.0).min(length.max(1.0));
    let half = head * 0.5;
    let (start_head, end_head) = match options.arrow {
        Arrow::None => (false, false),
        Arrow::Start => (true, false),
        Arrow::End => (false, true),
        Arrow::Both => (true, true),
    };
    // The shaft stops inside each head so its cap cannot poke past the tip.
    let trim = head * 0.8;
    let a = if start_head {
        (from.0 + dir.0 * trim, from.1 + dir.1 * trim)
    } else {
        from
    };
    let b = if end_head {
        (to.0 - dir.0 * trim, to.1 - dir.1 * trim)
    } else {
        to
    };
    let mut shaft = PathBuilder::new();
    shaft.move_to(a.0, a.1);
    // A zero-length segment still needs a direction to draw its round cap.
    shaft.line_to(b.0 + if a == b { 0.001 } else { 0.0 }, b.1);
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let shaft = shaft.finish()?.stroke(&stroke, 1.0)?;
    let mut out = fill_path(&shaft, area, options.antialias);
    let tips = [(start_head, from, (-dir.0, -dir.1)), (end_head, to, dir)];
    for (wanted, tip, direction) in tips {
        if !wanted {
            continue;
        }
        let Some(path) = arrow_head(tip, direction, head, half) else {
            continue;
        };
        let Some(mask) = fill_path(&path, area, options.antialias) else {
            continue;
        };
        out = Some(match out {
            Some(existing) => union(existing, &mask),
            None => mask,
        });
    }
    out
}

/// The `max` of two masks over the union of their rectangles.
fn union(a: Coverage, b: &Coverage) -> Coverage {
    let mut out = Coverage::new(a.rect().union(b.rect()));
    let stride = out.rect().width() as usize;
    let origin = out.rect();
    for mask in [&a, b] {
        let r = mask.rect();
        for y in r.y0..r.y1 {
            let row = mask.row(y, r.x0, r.x1);
            let at = (y - origin.y0) as usize * stride + (r.x0 - origin.x0) as usize;
            for (slot, &v) in out.data_mut()[at..at + row.len()].iter_mut().zip(row) {
                *slot = (*slot).max(v);
            }
        }
    }
    out
}
