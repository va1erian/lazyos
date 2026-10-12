#![forbid(unsafe_code)]

//! How a stroke's colour lands on a layer when it is not fully opaque.
//!
//! The raster routines write their colour over what is there. For a colour
//! with alpha that would replace the picture with a translucent pixel instead
//! of laying the colour over it, so brushes and shapes draw a mask into a
//! scratch bitmap and composite it with straight-alpha source-over against the
//! pixel the stroke found (not the one a previous dab of the same stroke
//! left): overlapping dabs do not build up, however slowly the pointer moves.
//!
//! What overwrites on purpose: the pencil (exact colour, alpha included, as
//! pixel artists expect), the eraser's primary button (alpha 0 is the point)
//! and the bucket (it replaces a region).

use crate::model::history::UndoCapture;
use crate::model::{Bitmap, Pixel, Rect, Tool, source_over};

/// The opaque marker the scratch mask is drawn with.
const MARK: Pixel = [0, 0, 0, 255];

/// Whether `tool` painting `color` composites over the layer rather than
/// replacing its pixels.
pub(super) fn blends(tool: Tool, color: Pixel) -> bool {
    match tool {
        Tool::Pencil => false,
        Tool::Eraser => color[3] != 0 && color[3] != 255,
        _ => color[3] != 255,
    }
}

/// Composites `color` over the pixels of `area` that `draw` marks.
///
/// `draw` receives a scratch bitmap the size of `area` and the offset to add
/// to picture coordinates to land in it, and must draw with the colour it is
/// given. `capture` must already have touched `area`.
pub(super) fn paint_over(
    target: &mut Bitmap,
    capture: &UndoCapture,
    area: Rect,
    color: Pixel,
    draw: impl FnOnce(&mut Bitmap, (i32, i32), Pixel),
) {
    let area = area.intersect(target.rect());
    if area.is_empty() {
        return;
    }
    let mut mask = Bitmap::transparent(area.width(), area.height());
    draw(
        &mut mask,
        (area.x0.saturating_neg(), area.y0.saturating_neg()),
        MARK,
    );
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            if mask
                .get(x - area.x0, y - area.y0)
                .is_some_and(|m| m[3] != 0)
                && let Some(old) = capture.original(target, x, y)
            {
                target.put(x, y, source_over(old, color, 255));
            }
        }
    }
}

#[cfg(test)]
mod tests;
