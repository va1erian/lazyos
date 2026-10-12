#![forbid(unsafe_code)]

//! Gradients from the primary to the secondary colour, alpha included.

use crate::model::bitmap::{Bitmap, Pixel};
use crate::model::blend::source_over;
use crate::model::rect::Rect;

use super::coverage::Clip;
use super::options::{GradientKind, GradientOptions};

/// A gradient between two points in continuous image coordinates (pixel
/// `(x, y)` is sampled at its centre `(x + 0.5, y + 0.5)`).
#[derive(Clone, Copy, Debug)]
pub struct Gradient {
    /// The shape.
    pub kind: GradientKind,
    /// Where the primary colour is: the line's start or the centre.
    pub start: (f32, f32),
    /// Where the secondary colour is reached.
    pub end: (f32, f32),
    /// The colour at `t = 0`.
    pub primary: Pixel,
    /// The colour at `t = 1`.
    pub secondary: Pixel,
}

impl Gradient {
    /// A gradient from `options`, the drag `start`..`end` and the colours.
    pub fn new(
        options: GradientOptions,
        start: (f32, f32),
        end: (f32, f32),
        primary: Pixel,
        secondary: Pixel,
    ) -> Gradient {
        Gradient {
            kind: options.kind,
            start,
            end,
            primary,
            secondary,
        }
    }

    /// The position along the gradient, 0 at the primary colour and 1 at the
    /// secondary one and beyond, for the point `(px, py)`.
    pub fn parameter(&self, px: f32, py: f32) -> f32 {
        let (ax, ay) = (self.end.0 - self.start.0, self.end.1 - self.start.1);
        let length = (ax * ax + ay * ay).sqrt();
        if length < 1.0e-6 {
            // No drag: everything is past the end.
            return 1.0;
        }
        let (dx, dy) = (px - self.start.0, py - self.start.1);
        // Along and across the drag.
        let along = (dx * ax + dy * ay) / length;
        let across = (dy * ax - dx * ay) / length;
        let t = match self.kind {
            GradientKind::Linear => along / length,
            GradientKind::LinearReflected => along.abs() / length,
            GradientKind::Radial => (dx * dx + dy * dy).sqrt() / length,
            GradientKind::Diamond => (along.abs() + across.abs()) / length,
        };
        t.clamp(0.0, 1.0)
    }

    /// The colour at the pixel `(x, y)`.
    pub fn at(&self, x: i32, y: i32) -> Pixel {
        mix(
            self.primary,
            self.secondary,
            self.parameter(x as f32 + 0.5, y as f32 + 0.5),
        )
    }

    /// Paints the gradient over `rect` of `layer` (source-over, so a gradient
    /// with transparent ends shows the layer through) and returns the pixels
    /// it changed. `clip` is a selection's mask: coverage scales the
    /// gradient's opacity and pixels outside its rectangle are untouched.
    pub fn paint(&self, layer: &mut Bitmap, rect: Rect, clip: Option<Clip<'_>>) -> Rect {
        let mut area = rect.intersect(layer.rect());
        if let Some(clip) = clip {
            area = area.intersect(clip.rect);
        }
        if area.is_empty() {
            return Rect::EMPTY;
        }
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let opacity = clip.map_or(255, |c| c.get(x, y));
                if opacity == 0 {
                    continue;
                }
                if let Some(under) = layer.get(x, y) {
                    layer.put(x, y, source_over(under, self.at(x, y), opacity));
                }
            }
        }
        area
    }
}

/// Linear interpolation of straight RGBA, weighting colour by alpha so a
/// transparent end contributes no colour of its own. Exact at `t` of 0 and 1.
pub fn mix(a: Pixel, b: Pixel, t: f32) -> Pixel {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let alpha = f32::from(a[3]) * (1.0 - t) + f32::from(b[3]) * t;
    if alpha < 0.5 {
        return [a[0], a[1], a[2], 0];
    }
    let channel = |i: usize| -> u8 {
        let weighted =
            f32::from(a[i]) * f32::from(a[3]) * (1.0 - t) + f32::from(b[i]) * f32::from(b[3]) * t;
        (weighted / alpha + 0.5).clamp(0.0, 255.0) as u8
    };
    [channel(0), channel(1), channel(2), (alpha + 0.5) as u8]
}
