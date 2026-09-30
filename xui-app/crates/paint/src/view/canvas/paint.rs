#![forbid(unsafe_code)]

//! The canvas painter: the bitmap, the shape preview and the brush ring.

use xui_core::Theme;
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};

use super::super::color_of;
use super::CanvasState;
use crate::model::{Preview, PreviewKind};

pub(super) fn paint(canvas: &mut dyn Canvas, state: &CanvasState, theme: &Theme) {
    let bounds = canvas.bounds();
    canvas.clear(theme.background);
    canvas.push_clip(bounds);

    if let Some(image) = &state.image {
        let width = image.width() as i32;
        let height = image.height() as i32;
        let left = bounds.left - state.offset.0;
        let top = bounds.top - state.offset.1;
        canvas.draw_image(image, Rect::new(left, top, left + width, top + height));
    }
    if let Some(preview) = state.preview {
        draw_preview(canvas, bounds, state.offset, &preview);
    }
    if let Some(cursor) = state.cursor {
        let center = Point::new(
            bounds.left + cursor.0 - state.offset.0,
            bounds.top + cursor.1 - state.offset.1,
        );
        // xui gap: G4 — `Cursor` has no crosshair and the offscreen backend
        // ignores cursors, so the canvas paints its own brush ring.
        let radius = (state.brush.max(1) as f32 / 2.0).max(1.0);
        canvas.stroke_ellipse(center, radius, radius, theme.text, 1.0);
    }
    canvas.stroke_rect(bounds, theme.border, 1.0);
    canvas.pop_clip();
}

/// Draws a rubber-banded shape in canvas coordinates, mapped into the node.
fn draw_preview(canvas: &mut dyn Canvas, bounds: Rect, offset: (i32, i32), preview: &Preview) {
    let at = |point: (i32, i32)| {
        Point::new(
            bounds.left + point.0 - offset.0,
            bounds.top + point.1 - offset.1,
        )
    };
    let color = color_of(preview.color);
    let from = at(preview.from);
    let to = at(preview.to);
    match preview.kind {
        PreviewKind::Line => canvas.draw_line(from, to, color, preview.diameter as f32),
        PreviewKind::Rectangle => {
            let rect = Rect::new(
                from.x.min(to.x),
                from.y.min(to.y),
                from.x.max(to.x).max(from.x.min(to.x) + 1),
                from.y.max(to.y).max(from.y.min(to.y) + 1),
            );
            canvas.stroke_rect(rect, color, preview.diameter.max(1) as f32);
        }
        PreviewKind::Ellipse => {
            let cx = (from.x + to.x) as f32 / 2.0;
            let cy = (from.y + to.y) as f32 / 2.0;
            let rx = (to.x - from.x).unsigned_abs() as f32 / 2.0;
            let ry = (to.y - from.y).unsigned_abs() as f32 / 2.0;
            canvas.stroke_ellipse(
                Point::new(cx.round() as i32, cy.round() as i32),
                rx.max(1.0),
                ry.max(1.0),
                color,
                preview.diameter.max(1) as f32,
            );
        }
    }
}
