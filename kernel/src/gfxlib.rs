//! A tiny hand-written 2D rasterizer, used to benchmark against tiny-skia.
//!
//! Implements the same scene primitives the tiny-skia demo uses: a banded
//! backdrop, anti-aliased filled circles with alpha, and an anti-aliased thick
//! polyline. Everything draws directly into a [`Surface`].

use crate::gfx::Color;
use crate::surface::Surface;

/// Draw an approximation of the tiny-skia demo scene inside a rectangle.
pub fn scene(surface: &mut impl Surface, x: i32, y: i32, w: i32, h: i32) {
    let (wf, hf) = (w as f32, h as f32);

    // Banded vertical backdrop (solid fills).
    let bands = 96;
    let top = (9.0, 12.0, 34.0);
    let mid = (32.0, 16.0, 68.0);
    let bottom = (6.0, 42.0, 54.0);
    for i in 0..bands {
        let t = i as f32 / (bands - 1) as f32;
        let (a, b, s) = if t < 0.5 {
            (top, mid, t * 2.0)
        } else {
            (mid, bottom, (t - 0.5) * 2.0)
        };
        let color = Color::rgb(
            lerp(a.0, b.0, s) as u8,
            lerp(a.1, b.1, s) as u8,
            lerp(a.2, b.2, s) as u8,
        );
        let yy = y + (hf * i as f32 / bands as f32) as i32;
        let y2 = y + libm::ceilf(hf * (i + 1) as f32 / bands as f32) as i32 + 1;
        surface.fill_rect(x, yy, x + w, y2, color);
    }

    // Soft "sun": concentric translucent AA circles.
    let cx = x as f32 + wf * 0.5;
    let cy = y as f32 + hf * 0.40;
    for i in (1..=14).rev() {
        let radius = hf * 0.28 * (i as f32 / 14.0);
        let alpha = (14 + (14 - i) * 12) as u8;
        fill_circle(surface, cx, cy, radius, Color::rgb(255, 196, 96), alpha);
    }

    // Overlapping translucent circles.
    let dots = [
        (0.16f32, (239u8, 71u8, 111u8)),
        (0.39, (255, 209, 102)),
        (0.61, (6, 214, 160)),
        (0.84, (17, 138, 178)),
    ];
    for (fx, (r, g, b)) in dots {
        fill_circle(
            surface,
            x as f32 + wf * fx,
            y as f32 + hf * 0.72,
            hf * 0.15,
            Color::rgb(r, g, b),
            210,
        );
    }

    // Anti-aliased thick zigzag.
    let mut prev = (x as f32, y as f32 + hf * 0.88);
    let segments = 12;
    for i in 1..=segments {
        let nx = x as f32 + wf * i as f32 / segments as f32;
        let ny = if i % 2 == 0 {
            y as f32 + hf * 0.83
        } else {
            y as f32 + hf * 0.93
        };
        stroke_segment(surface, prev, (nx, ny), 4.0, Color::rgb(236, 236, 255), 220);
        prev = (nx, ny);
    }
}

/// Anti-aliased filled circle composited with `alpha`.
pub fn fill_circle(surface: &mut impl Surface, cx: f32, cy: f32, r: f32, color: Color, alpha: u8) {
    let (fw, fh) = (surface.width() as i32, surface.height() as i32);
    let x0 = ((cx - r - 1.0) as i32).max(0);
    let x1 = ((cx + r + 1.0) as i32).min(fw - 1);
    let y0 = ((cy - r - 1.0) as i32).max(0);
    let y1 = ((cy + r + 1.0) as i32).min(fh - 1);
    let a = alpha as f32 / 255.0;

    for yy in y0..=y1 {
        for xx in x0..=x1 {
            let dx = xx as f32 + 0.5 - cx;
            let dy = yy as f32 + 0.5 - cy;
            let dist = libm::sqrtf(dx * dx + dy * dy);
            let coverage = ((r + 0.5 - dist).clamp(0.0, 1.0)) * a;
            if coverage > 0.002 {
                surface.blend_pixel(xx as usize, yy as usize, color, (coverage * 255.0) as u8);
            }
        }
    }
}

/// Anti-aliased line segment of the given width.
fn stroke_segment(
    surface: &mut impl Surface,
    a: (f32, f32),
    b: (f32, f32),
    width: f32,
    color: Color,
    alpha: u8,
) {
    let half = width * 0.5;
    let (fw, fh) = (surface.width() as i32, surface.height() as i32);
    let x0 = (a.0.min(b.0) - half - 1.0) as i32;
    let x1 = (a.0.max(b.0) + half + 1.0) as i32;
    let y0 = (a.1.min(b.1) - half - 1.0) as i32;
    let y1 = (a.1.max(b.1) + half + 1.0) as i32;
    let abx = b.0 - a.0;
    let aby = b.1 - a.1;
    let len2 = abx * abx + aby * aby;
    let a_alpha = alpha as f32 / 255.0;

    for yy in y0.max(0)..=y1.min(fh - 1) {
        for xx in x0.max(0)..=x1.min(fw - 1) {
            let px = xx as f32 + 0.5;
            let py = yy as f32 + 0.5;
            let t = if len2 > 0.0 {
                (((px - a.0) * abx + (py - a.1) * aby) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let projx = a.0 + t * abx;
            let projy = a.1 + t * aby;
            let dx = px - projx;
            let dy = py - projy;
            let dist = libm::sqrtf(dx * dx + dy * dy);
            let coverage = ((half + 0.5 - dist).clamp(0.0, 1.0)) * a_alpha;
            if coverage > 0.002 {
                surface.blend_pixel(xx as usize, yy as usize, color, (coverage * 255.0) as u8);
            }
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
