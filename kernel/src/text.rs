//! Draw text at arbitrary positions onto any [`Surface`] using the build-time
//! glyph atlas. Unlike the console, this has no cursor state and supports a
//! clip rectangle, so text can be rendered inside windows or back buffers.

use crate::font::{self, COVERAGE, GLYPHS};
use crate::gfx::Color;
use crate::surface::Surface;

/// A clip rectangle (half-open: `x0 <= x < x1`).
#[derive(Clone, Copy)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// Draw `text` with its pen starting at `x` and the baseline at `baseline_y`.
/// Pixels outside `clip` are skipped.
pub fn draw_text(
    surface: &mut impl Surface,
    x: i32,
    baseline_y: i32,
    text: &str,
    color: Color,
    clip: Rect,
) {
    let mut pen = x;
    for ch in text.chars() {
        if pen >= clip.x1 {
            break;
        }
        draw_glyph(surface, pen, baseline_y, ch, color, clip);
        pen += font::ADVANCE as i32;
    }
}

fn draw_glyph(
    surface: &mut impl Surface,
    pen: i32,
    baseline_y: i32,
    ch: char,
    color: Color,
    clip: Rect,
) {
    let code = ch as u32;
    if code < font::FIRST_CHAR as u32 || code > font::LAST_CHAR as u32 {
        return;
    }
    let glyph = &GLYPHS[(code - font::FIRST_CHAR as u32) as usize];
    let (gw, gh) = (glyph.width as usize, glyph.height as usize);
    if gw == 0 || gh == 0 {
        return;
    }
    let start = glyph.offset as usize;
    let coverage = &COVERAGE[start..start + gw * gh];
    let origin_x = pen + glyph.left;
    let origin_y = baseline_y + glyph.top;

    // Fast path: calculate intersection of glyph bounding box with clip rect.
    let y_start = origin_y.max(clip.y0);
    let y_end = (origin_y + gh as i32).min(clip.y1);
    if y_start >= y_end {
        return;
    }

    let x_start = origin_x.max(clip.x0);
    let x_end = (origin_x + gw as i32).min(clip.x1);
    if x_start >= x_end {
        return;
    }

    // Convert clipped screen coordinates to glyph-relative local bitmap indices.
    let row_start = (y_start - origin_y) as usize;
    let row_end = (y_end - origin_y) as usize;
    let col_start = (x_start - origin_x) as usize;
    let col_end = (x_end - origin_x) as usize;

    for row in row_start..row_end {
        let y = (origin_y + row as i32) as usize;
        // Precompute row offset into coverage bitmap outside column loop.
        let row_off = row * gw;
        for col in col_start..col_end {
            let alpha = coverage[row_off + col];
            if alpha == 0 {
                continue;
            }
            // `col_start..col_end` guarantees `x` is strictly within `clip.x0..clip.x1`.
            let x = (origin_x + col as i32) as usize;
            surface.blend_pixel(x, y, color, alpha);
        }
    }
}
