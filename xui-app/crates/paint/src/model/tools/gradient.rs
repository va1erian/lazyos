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
        match Axis::of(self) {
            None => 1.0,
            Some(axis) => axis.parameter(self.kind, &axis.row(self.start, py), px - self.start.0),
        }
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
    ///
    /// Every pixel gets exactly the colour [`Gradient::at`] gives, but a row
    /// is worked on as a slice, with what depends only on the row computed
    /// once: this runs on every move of a drag over the whole layer.
    pub fn paint(&self, layer: &mut Bitmap, rect: Rect, clip: Option<Clip<'_>>) -> Rect {
        let mut area = rect.intersect(layer.rect());
        if let Some(clip) = clip {
            area = area.intersect(clip.rect());
        }
        if area.is_empty() {
            return Rect::EMPTY;
        }
        let axis = Axis::of(self);
        let colors = Mixer::new(self.primary, self.secondary);
        let width = area.width() as usize;
        let stride = layer.width() as usize * 4;
        for y in area.y0..area.y1 {
            let opacity: Option<&[u8]> = match clip {
                Some(clip @ Clip::Mask { .. }) => match clip.span(y, area.x0, area.x1) {
                    Some(span) => Some(span),
                    // A row the mask does not describe is left alone.
                    None => continue,
                },
                _ => None,
            };
            let row = layer.pixels_mut();
            let at = y as usize * stride + area.x0 as usize * 4;
            let out = row[at..at + width * 4].as_chunks_mut::<4>().0;
            let line = axis.map(|axis| axis.row(self.start, y as f32 + 0.5));
            for (i, under) in out.iter_mut().enumerate() {
                let cover = opacity.map_or(255, |o| o[i]);
                if cover == 0 {
                    continue;
                }
                let t = match (&axis, &line) {
                    (Some(axis), Some(line)) => {
                        let px = (area.x0 + i as i32) as f32 + 0.5;
                        axis.parameter(self.kind, line, px - self.start.0)
                    }
                    _ => 1.0,
                };
                *under = source_over(*under, colors.at(t), cover);
            }
        }
        area
    }
}

/// The drag as numbers: its vector and length, `None` when it has no length
/// (everything is then past the end).
#[derive(Clone, Copy)]
struct Axis {
    ax: f32,
    ay: f32,
    /// `1 / length`.
    inv_length: f32,
    /// `1 / length^2`.
    inv_square: f32,
}

/// What depends on the row alone, for [`Axis::parameter`].
struct Row {
    /// `dy * ay`.
    dy_ay: f32,
    /// `dy * ax`.
    dy_ax: f32,
    /// `dy * dy`.
    dy_sq: f32,
}

impl Axis {
    fn of(gradient: &Gradient) -> Option<Axis> {
        let (ax, ay) = (
            gradient.end.0 - gradient.start.0,
            gradient.end.1 - gradient.start.1,
        );
        let length = (ax * ax + ay * ay).sqrt();
        (length >= 1.0e-6).then(|| Axis {
            ax,
            ay,
            inv_length: 1.0 / length,
            inv_square: 1.0 / (length * length),
        })
    }

    /// The row terms for the sample row `py`.
    fn row(&self, start: (f32, f32), py: f32) -> Row {
        let dy = py - start.1;
        Row {
            dy_ay: dy * self.ay,
            dy_ax: dy * self.ax,
            dy_sq: dy * dy,
        }
    }

    /// The position for a sample `dx` right of the start on a row. Reciprocals
    /// of the length replace the divisions: this is the per-pixel path.
    #[inline]
    fn parameter(&self, kind: GradientKind, row: &Row, dx: f32) -> f32 {
        // The drag's dot product with the sample (along, times the length)
        // and its cross product (across, times the length).
        let along = dx * self.ax + row.dy_ay;
        let t = match kind {
            GradientKind::Linear => along * self.inv_square,
            GradientKind::LinearReflected => along.abs() * self.inv_square,
            GradientKind::Radial => (dx * dx + row.dy_sq).sqrt() * self.inv_length,
            GradientKind::Diamond => {
                let across = row.dy_ax - dx * self.ay;
                (along.abs() + across.abs()) * self.inv_square
            }
        };
        t.clamp(0.0, 1.0)
    }
}

/// The two end colours as the interpolation wants them, worked out once.
struct Mixer {
    a: Pixel,
    b: Pixel,
    /// Each channel times its alpha: colour weighted by how much of it there is.
    a_weighted: [f32; 3],
    b_weighted: [f32; 3],
}

impl Mixer {
    fn new(a: Pixel, b: Pixel) -> Mixer {
        let weigh =
            |p: Pixel| -> [f32; 3] { std::array::from_fn(|i| f32::from(p[i]) * f32::from(p[3])) };
        Mixer {
            a,
            b,
            a_weighted: weigh(a),
            b_weighted: weigh(b),
        }
    }

    /// The colour at `t`, see [`mix`].
    #[inline]
    fn at(&self, t: f32) -> Pixel {
        if t <= 0.0 {
            return self.a;
        }
        if t >= 1.0 {
            return self.b;
        }
        let alpha = f32::from(self.a[3]) * (1.0 - t) + f32::from(self.b[3]) * t;
        if alpha < 0.5 {
            return [self.a[0], self.a[1], self.a[2], 0];
        }
        let scale = 1.0 / alpha;
        let channel = |i: usize| -> u8 {
            let weighted = self.a_weighted[i] * (1.0 - t) + self.b_weighted[i] * t;
            (weighted * scale + 0.5).clamp(0.0, 255.0) as u8
        };
        [channel(0), channel(1), channel(2), (alpha + 0.5) as u8]
    }
}

/// Linear interpolation of straight RGBA, weighting colour by alpha so a
/// transparent end contributes no colour of its own. Exact at `t` of 0 and 1.
pub fn mix(a: Pixel, b: Pixel, t: f32) -> Pixel {
    Mixer::new(a, b).at(t)
}
