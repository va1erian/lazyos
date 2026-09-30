#![forbid(unsafe_code)]

//! The tool-strip glyphs.
//!
//! xui gap: G7 — the vendored Lucide set has no brush, eraser, fill, dropper,
//! line or ellipse, so these six are drawn by hand.
//!
//! xui's vendored Lucide set has no paint tools, so the shape tools are drawn
//! here from canvas primitives; the actions reuse Lucide where an outline
//! exists (`Undo2`, `Redo2`, `Trash2`, `FilePlus`, `Save`, `FolderOpen`).

use xui_core::Color;
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};
use xui_core::icon::{Lucide, draw_icon};

use super::StripItem;
use crate::model::Tool;

/// Draws the glyph for one strip item centred in `rect`.
pub(crate) fn draw(canvas: &mut dyn Canvas, item: StripItem, rect: Rect, color: Color, dpi: u32) {
    match item {
        StripItem::Tool(tool) => draw_tool(canvas, tool, rect, color, dpi),
        StripItem::Size(size) => draw_size(canvas, size, rect, color, dpi),
        StripItem::Undo => draw_icon(canvas, Lucide::Undo2, rect, color, dpi),
        StripItem::Redo => draw_icon(canvas, Lucide::Redo2, rect, color, dpi),
        StripItem::Clear => draw_icon(canvas, Lucide::Trash2, rect, color, dpi),
        StripItem::New => draw_icon(canvas, Lucide::FilePlus, rect, color, dpi),
        StripItem::Save => draw_icon(canvas, Lucide::Save, rect, color, dpi),
        StripItem::Open => draw_icon(canvas, Lucide::FolderOpen, rect, color, dpi),
        StripItem::Resize => draw_resize(canvas, rect, color, dpi),
    }
}

/// Insets a cell into its icon box, keeping at least a 4px square.
fn inset(rect: Rect, amount: i32) -> Rect {
    let box_side = rect.width().min(rect.height()).max(4);
    let side = (box_side - amount * 2).max(4);
    let left = rect.left + (rect.width() - side) / 2;
    let top = rect.top + (rect.height() - side) / 2;
    Rect::new(left, top, left + side, top + side)
}

/// Draws one tool's glyph by hand.
fn draw_tool(canvas: &mut dyn Canvas, tool: Tool, rect: Rect, color: Color, dpi: u32) {
    let box_rect = inset(rect, 6);
    let stroke = (1.5 * dpi as f32 / 96.0).max(1.0);
    match tool {
        Tool::Pencil => draw_icon(canvas, Lucide::Pencil, rect, color, dpi),
        Tool::Rectangle => draw_icon(canvas, Lucide::Square, rect, color, dpi),
        Tool::Picker => draw_icon(canvas, Lucide::MousePointer2, rect, color, dpi),
        Tool::Line => canvas.draw_line(
            Point::new(box_rect.left, box_rect.bottom),
            Point::new(box_rect.right, box_rect.top),
            color,
            stroke,
        ),
        Tool::Ellipse => canvas.stroke_ellipse(
            Point::new(
                (box_rect.left + box_rect.right) / 2,
                (box_rect.top + box_rect.bottom) / 2,
            ),
            box_rect.width() as f32 / 2.0,
            box_rect.height() as f32 / 2.0,
            color,
            stroke,
        ),
        Tool::Brush => {
            // A handle from the bottom-left to a round head at the top-right.
            canvas.draw_line(
                Point::new(box_rect.left, box_rect.bottom),
                Point::new(
                    box_rect.right - box_rect.width() / 5,
                    box_rect.top + box_rect.height() / 5,
                ),
                color,
                stroke * 2.0,
            );
            canvas.fill_ellipse(
                Point::new(
                    box_rect.right - box_rect.width() / 8,
                    box_rect.top + box_rect.height() / 8,
                ),
                box_rect.width() as f32 / 4.0,
                box_rect.height() as f32 / 4.0,
                color,
            );
        }
        Tool::Eraser => {
            // A slanted eraser: a rounded rectangle drawn as a filled quad.
            let points = [
                Point::new(box_rect.left, box_rect.bottom - box_rect.height() / 3),
                Point::new(box_rect.left + box_rect.width() / 3, box_rect.bottom),
                Point::new(box_rect.right, box_rect.bottom - box_rect.height() / 3),
                Point::new(
                    box_rect.right - box_rect.width() / 3,
                    box_rect.bottom - box_rect.height() * 2 / 3,
                ),
            ];
            canvas.fill_polygon(&points, color);
        }
        Tool::Fill => {
            // A bucket: a tilted quad plus a drop.
            let points = [
                Point::new(box_rect.left, box_rect.top + box_rect.height() / 2),
                Point::new(box_rect.left + box_rect.width() / 2, box_rect.top),
                Point::new(box_rect.right, box_rect.top + box_rect.height() / 2),
                Point::new(box_rect.left + box_rect.width() / 2, box_rect.bottom),
            ];
            canvas.fill_polygon(&points, color);
            canvas.draw_line(
                Point::new(box_rect.left, box_rect.top + box_rect.height() / 2),
                Point::new(
                    box_rect.left - box_rect.width() / 6,
                    box_rect.top + box_rect.height() * 3 / 4,
                ),
                color,
                stroke,
            );
        }
    }
}

/// Draws the resize glyph (Lucide has none): a frame with a handle square in
/// its bottom-right corner and a diagonal towards it.
fn draw_resize(canvas: &mut dyn Canvas, rect: Rect, color: Color, dpi: u32) {
    let frame = inset(rect, 6);
    let stroke = (1.5 * dpi as f32 / 96.0).max(1.0);
    canvas.stroke_rect(frame, color, stroke);
    let handle = (frame.width() / 3).max(2);
    canvas.fill_rect(
        Rect::new(
            frame.right - handle,
            frame.bottom - handle,
            frame.right,
            frame.bottom,
        ),
        color,
    );
    canvas.draw_line(
        Point::new(
            frame.left + frame.width() / 4,
            frame.top + frame.height() / 4,
        ),
        Point::new(frame.right - handle, frame.bottom - handle),
        color,
        stroke,
    );
}

/// Draws a size swatch: the dot the brush would paint.
fn draw_size(canvas: &mut dyn Canvas, size: u32, rect: Rect, color: Color, dpi: u32) {
    let box_rect = inset(rect, 4);
    let largest = crate::model::SIZES[crate::model::SIZES.len() - 1].max(1) as f32;
    let scale = (box_rect.width().min(box_rect.height()) as f32 / largest).max(1.0);
    let radius = (size as f32 * scale / 2.0).clamp(1.0, box_rect.width() as f32 / 2.0);
    let center = Point::new(
        (box_rect.left + box_rect.right) / 2,
        (box_rect.top + box_rect.bottom) / 2,
    );
    canvas.fill_ellipse(center, radius, radius, color);
    let _ = dpi;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insetting_a_small_cell_keeps_a_drawable_box() {
        let rect = inset(Rect::new(0, 0, 2, 2), 6);
        assert!(rect.width() >= 4 && rect.height() >= 4);
    }
}
