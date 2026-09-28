#![forbid(unsafe_code)]

//! The portable [`Canvas`] over a `tiny-skia` pixmap.
//!
//! Shapes are drawn with a tracked translation/scale and a soft clip (each
//! shape's rectangle is intersected with the current clip), matching the Win32
//! backend. Text rasterisation is not implemented yet, so [`Canvas::draw_text`]
//! draws nothing until the text system lands.

use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Shader, Stroke, Transform};

use xui_core::backend::{Canvas, TextStyle};
use xui_core::color::Color;
use xui_core::geometry::{Point, Rect};

use crate::to_skia;

/// A drawing surface over a `tiny-skia` pixmap, clipped to `bounds`.
pub struct SkiaCanvas<'a> {
    pixmap: &'a mut Pixmap,
    bounds: Rect,
    dpi: u32,
    tx: f32,
    ty: f32,
    scale: f32,
    clips: Vec<Rect>,
    saved: Vec<(f32, f32, f32)>,
}

impl<'a> SkiaCanvas<'a> {
    pub(crate) fn new(pixmap: &'a mut Pixmap, bounds: Rect, dpi: u32) -> SkiaCanvas<'a> {
        SkiaCanvas {
            pixmap,
            bounds,
            dpi,
            tx: 0.0,
            ty: 0.0,
            scale: 1.0,
            clips: Vec::new(),
            saved: Vec::new(),
        }
    }

    fn point(&self, point: Point) -> Point {
        Point::new(
            (self.tx + point.x as f32 * self.scale).round() as i32,
            (self.ty + point.y as f32 * self.scale).round() as i32,
        )
    }

    fn rect(&self, rect: Rect) -> Rect {
        let mapped = Rect::new(
            (self.tx + rect.left as f32 * self.scale).round() as i32,
            (self.ty + rect.top as f32 * self.scale).round() as i32,
            (self.tx + rect.right as f32 * self.scale).round() as i32,
            (self.ty + rect.bottom as f32 * self.scale).round() as i32,
        );
        self.clips
            .last()
            .copied()
            .map_or(mapped, |clip| intersect(mapped, clip))
    }

    /// Fills `path`, whose coordinates are already in device space (the
    /// transform is applied by [`SkiaCanvas::rect`]/[`SkiaCanvas::point`]).
    fn fill(&mut self, path: &Path, color: Color) {
        let paint = Paint {
            shader: Shader::SolidColor(to_skia(color)),
            anti_alias: true,
            ..Paint::default()
        };
        self.pixmap
            .fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    fn stroke(&mut self, path: &Path, color: Color, width: f32) {
        let paint = Paint {
            shader: Shader::SolidColor(to_skia(color)),
            anti_alias: true,
            ..Paint::default()
        };
        let stroke = Stroke {
            width: (width * self.scale).max(1.0),
            ..Stroke::default()
        };
        self.pixmap
            .stroke_path(path, &paint, &stroke, Transform::identity(), None);
    }
}

fn rect_path(rect: Rect) -> Option<Path> {
    if rect.is_empty() {
        return None;
    }
    Some(PathBuilder::from_rect(sk_rect(rect)))
}

/// A rounded-rectangle path: four straight edges joined by quadratic corners.
fn round_rect_path(rect: Rect, radius: f32) -> Option<Path> {
    if rect.is_empty() {
        return None;
    }
    let (l, t) = (rect.left as f32, rect.top as f32);
    let (r, b) = (rect.right as f32, rect.bottom as f32);
    let radius = radius.max(0.0).min((r - l) / 2.0).min((b - t) / 2.0);
    let mut builder = PathBuilder::new();
    builder.move_to(l + radius, t);
    builder.line_to(r - radius, t);
    builder.quad_to(r, t, r, t + radius);
    builder.line_to(r, b - radius);
    builder.quad_to(r, b, r - radius, b);
    builder.line_to(l + radius, b);
    builder.quad_to(l, b, l, b - radius);
    builder.line_to(l, t + radius);
    builder.quad_to(l, t, l + radius, t);
    builder.close();
    builder.finish()
}

fn sk_rect(rect: Rect) -> tiny_skia::Rect {
    tiny_skia::Rect::from_ltrb(
        rect.left as f32,
        rect.top as f32,
        rect.right as f32,
        rect.bottom as f32,
    )
    .unwrap_or_else(|| tiny_skia::Rect::from_ltrb(0.0, 0.0, 0.0, 0.0).expect("empty rect"))
}

fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

impl Canvas for SkiaCanvas<'_> {
    fn dpi(&self) -> u32 {
        self.dpi
    }

    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn clear(&mut self, color: Color) {
        self.fill_rect(self.bounds, color);
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        if let Some(path) = rect_path(self.rect(rect)) {
            self.fill(&path, color);
        }
    }

    fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        let rect = self.rect(rect);
        if let Some(path) = round_rect_path(rect, radius * self.scale) {
            self.fill(&path, color);
        }
    }

    fn fill_ellipse(&mut self, center: Point, radius_x: f32, radius_y: f32, color: Color) {
        let center = self.point(center);
        let rx = radius_x * self.scale;
        let ry = radius_y * self.scale;
        let bounds = Rect::new(
            (center.x as f32 - rx).round() as i32,
            (center.y as f32 - ry).round() as i32,
            (center.x as f32 + rx).round() as i32,
            (center.y as f32 + ry).round() as i32,
        );
        let bounds = self
            .clips
            .last()
            .copied()
            .map_or(bounds, |clip| intersect(bounds, clip));
        if bounds.is_empty() {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.push_oval(sk_rect(bounds));
        if let Some(path) = builder.finish() {
            self.fill(&path, color);
        }
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        if let Some(path) = rect_path(self.rect(rect)) {
            self.stroke(&path, color, width);
        }
    }

    fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color, width: f32) {
        let rect = self.rect(rect);
        if let Some(path) = round_rect_path(rect, radius * self.scale) {
            self.stroke(&path, color, width);
        }
    }

    fn stroke_ellipse(
        &mut self,
        center: Point,
        radius_x: f32,
        radius_y: f32,
        color: Color,
        width: f32,
    ) {
        let center = self.point(center);
        let bounds = Rect::new(
            (center.x as f32 - radius_x * self.scale).round() as i32,
            (center.y as f32 - radius_y * self.scale).round() as i32,
            (center.x as f32 + radius_x * self.scale).round() as i32,
            (center.y as f32 + radius_y * self.scale).round() as i32,
        );
        if bounds.is_empty() {
            return;
        }
        let mut builder = PathBuilder::new();
        builder.push_oval(sk_rect(bounds));
        if let Some(path) = builder.finish() {
            self.stroke(&path, color, width);
        }
    }

    fn draw_line(&mut self, from: Point, to: Point, color: Color, width: f32) {
        let (from, to) = (self.point(from), self.point(to));
        let mut builder = PathBuilder::new();
        builder.move_to(from.x as f32, from.y as f32);
        builder.line_to(to.x as f32, to.y as f32);
        if let Some(path) = builder.finish() {
            self.stroke(&path, color, width);
        }
    }

    fn draw_text(&mut self, text: &str, rect: Rect, style: &TextStyle) {
        let rect = self.rect(rect);
        let dpi = self.dpi;
        crate::text::draw(self.pixmap, text, rect, style, dpi);
    }

    fn push_clip(&mut self, rect: Rect) {
        let rect = self.rect(rect);
        let clipped = self
            .clips
            .last()
            .map_or(rect, |outer| intersect(*outer, rect));
        self.clips.push(clipped);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn save(&mut self) {
        self.saved.push((self.tx, self.ty, self.scale));
    }

    fn restore(&mut self) {
        if let Some((tx, ty, scale)) = self.saved.pop() {
            self.tx = tx;
            self.ty = ty;
            self.scale = scale;
        }
    }

    fn set_translation(&mut self, x: f32, y: f32) {
        self.tx = x;
        self.ty = y;
    }

    fn set_scale_translate(&mut self, scale: f32, x: f32, y: f32) {
        self.scale = scale;
        self.tx = x;
        self.ty = y;
    }
}
