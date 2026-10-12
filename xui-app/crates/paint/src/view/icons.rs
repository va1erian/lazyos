#![forbid(unsafe_code)]

//! The tool-grid glyphs.
//!
//! xui gap: G7 — the vendored Lucide set has no brush, eraser, fill, dropper,
//! line or ellipse, so those are drawn by hand; the pencil, rectangle and picker
//! reuse Lucide outlines.

use xui_core::Color;
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};
use xui_core::icon::{Lucide, draw_icon};

use crate::model::Tool;

/// Draws the glyph for `tool` centred in `rect`.
pub(crate) fn draw(canvas: &mut dyn Canvas, tool: Tool, rect: Rect, color: Color, dpi: u32) {
    draw_tool(canvas, tool, rect, color, dpi);
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
        Tool::RectSelect => dashed_rect(canvas, box_rect, color, stroke),
        Tool::EllipseSelect => dashed_ellipse(canvas, box_rect, color, stroke),
        Tool::Lasso => {
            // An open loop, as a lasso is drawn, with a tail.
            let (w, h) = (box_rect.width(), box_rect.height());
            let at = |fx: f32, fy: f32| {
                Point::new(
                    box_rect.left + (w as f32 * fx) as i32,
                    box_rect.top + (h as f32 * fy) as i32,
                )
            };
            let loop_points = [
                at(0.15, 0.55),
                at(0.2, 0.25),
                at(0.5, 0.1),
                at(0.8, 0.25),
                at(0.85, 0.5),
                at(0.6, 0.65),
                at(0.35, 0.65),
                at(0.25, 0.55),
            ];
            for pair in loop_points.windows(2) {
                canvas.draw_line(pair[0], pair[1], color, stroke);
            }
            canvas.draw_line(at(0.35, 0.65), at(0.3, 0.95), color, stroke);
        }
        Tool::MagicWand => {
            // A wand from the bottom-left with a spark at its tip.
            let tip = Point::new(
                box_rect.right - box_rect.width() / 4,
                box_rect.top + box_rect.height() / 4,
            );
            canvas.draw_line(
                Point::new(box_rect.left, box_rect.bottom),
                tip,
                color,
                stroke * 1.5,
            );
            let ray = (box_rect.width() / 4).max(2);
            for (dx, dy) in [(ray, 0), (-ray, 0), (0, ray), (0, -ray)] {
                canvas.draw_line(tip, Point::new(tip.x + dx, tip.y + dy), color, stroke);
            }
        }
        Tool::MovePixels => move_cross(canvas, box_rect, color, stroke, true),
        Tool::MoveSelection => move_cross(canvas, box_rect, color, stroke, false),
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

/// A rectangle outline made of dashes, the look of a selection.
fn dashed_rect(canvas: &mut dyn Canvas, rect: Rect, color: Color, stroke: f32) {
    let dash = (stroke * 2.0).round().max(2.0) as i32;
    let mut run = |from: Point, to: Point| {
        let (dx, dy) = ((to.x - from.x).signum(), (to.y - from.y).signum());
        let length = (to.x - from.x).abs().max((to.y - from.y).abs());
        let mut at = 0;
        while at < length {
            let end = (at + dash).min(length);
            canvas.draw_line(
                Point::new(from.x + dx * at, from.y + dy * at),
                Point::new(from.x + dx * end, from.y + dy * end),
                color,
                stroke,
            );
            at += dash * 2;
        }
    };
    run(
        Point::new(rect.left, rect.top),
        Point::new(rect.right, rect.top),
    );
    run(
        Point::new(rect.right, rect.top),
        Point::new(rect.right, rect.bottom),
    );
    run(
        Point::new(rect.right, rect.bottom),
        Point::new(rect.left, rect.bottom),
    );
    run(
        Point::new(rect.left, rect.bottom),
        Point::new(rect.left, rect.top),
    );
}

/// An ellipse outline made of dashes.
fn dashed_ellipse(canvas: &mut dyn Canvas, rect: Rect, color: Color, stroke: f32) {
    let (cx, cy) = (
        (rect.left + rect.right) as f32 / 2.0,
        (rect.top + rect.bottom) as f32 / 2.0,
    );
    let (rx, ry) = (rect.width() as f32 / 2.0, rect.height() as f32 / 2.0);
    let steps = 16;
    for step in (0..steps).step_by(2) {
        let point = |i: i32| {
            let angle = i as f32 / steps as f32 * std::f32::consts::TAU;
            Point::new(
                (cx + rx * angle.cos()).round() as i32,
                (cy + ry * angle.sin()).round() as i32,
            )
        };
        canvas.draw_line(point(step), point(step + 1), color, stroke);
    }
}

/// Four arrows from the centre; with `solid` a filled square in the middle
/// (the pixels), without a hollow one (only the outline).
fn move_cross(canvas: &mut dyn Canvas, rect: Rect, color: Color, stroke: f32, solid: bool) {
    let (cx, cy) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
    let reach = rect.width().min(rect.height()) / 2;
    let head = (reach / 3).max(2);
    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let tip = Point::new(cx + dx * reach, cy + dy * reach);
        canvas.draw_line(Point::new(cx, cy), tip, color, stroke);
        let back = Point::new(tip.x - dx * head, tip.y - dy * head);
        let side = Point::new(-dy * head, dx * head);
        canvas.fill_polygon(
            &[
                tip,
                Point::new(back.x + side.x, back.y + side.y),
                Point::new(back.x - side.x, back.y - side.y),
            ],
            color,
        );
    }
    let half = (reach / 4).max(1);
    let square = Rect::new(cx - half, cy - half, cx + half, cy + half);
    if solid {
        canvas.fill_rect(square, color);
    } else {
        canvas.stroke_rect(square, color, stroke);
    }
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
