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

    for row in 0..gh {
        let y = origin_y + row as i32;
        if y < clip.y0 || y >= clip.y1 {
            continue;
        }
        for col in 0..gw {
            let alpha = coverage[row * gw + col];
            if alpha == 0 {
                continue;
            }
            let x = origin_x + col as i32;
            if x < clip.x0 || x >= clip.x1 {
                continue;
            }
            surface.blend_pixel(x as usize, y as usize, color, alpha);
        }
    }
}
