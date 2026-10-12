#![forbid(unsafe_code)]

//! The canvas painter: the screen image 1:1, the shape preview and the brush
//! ring.

use xui_core::Theme;
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};

use super::super::color_of;
use super::state::CanvasState;
use crate::model::{Preview, PreviewKind};

pub(super) fn paint(canvas: &mut dyn Canvas, state: &mut CanvasState, theme: &Theme) {
    let bounds = canvas.bounds();
    state.set_view(bounds);
    canvas.clear(theme.background);
    canvas.push_clip(bounds);

    // The screen image is already scaled to the zoom and covers only what is
    // visible, so it is drawn 1:1: xui's bilinear filter has nothing to blur.
    if let Some(image) = state.screen() {
        let (width, height) = (image.width() as i32, image.height() as i32);
        canvas.draw_image(
            image,
            Rect::new(
                bounds.left,
                bounds.top,
                bounds.left + width,
                bounds.top + height,
            ),
        );
    }
    let zoom = state.viewport().zoom();
    // The view position of the middle of picture pixel `point`.
    let at = |point: (i32, i32)| {
        let (x, y) = state.viewport().to_view(point.0, point.1);
        let half = (zoom / 2.0).round() as i32;
        Point::new(bounds.left + x + half, bounds.top + y + half)
    };
    if let Some(preview) = state.preview {
        draw_preview(canvas, &at, zoom, &preview);
    }
    if let Some(cursor) = state.cursor {
        // xui gap: G4 — `Cursor` has no crosshair and the offscreen backend
        // ignores cursors, so the canvas paints its own brush ring.
        let radius = (state.brush.max(1) as f32 * zoom as f32 / 2.0).max(1.0);
        canvas.stroke_ellipse(at(cursor), radius, radius, theme.text, 1.0);
    }
    canvas.stroke_rect(bounds, theme.border, 1.0);
    canvas.pop_clip();
}

/// Draws a rubber-banded shape in picture coordinates mapped through `at`.
fn draw_preview(
    canvas: &mut dyn Canvas,
    at: &dyn Fn((i32, i32)) -> Point,
    zoom: f64,
    preview: &Preview,
) {
    let color = color_of(preview.color);
    let from = at(preview.from);
    let to = at(preview.to);
    let thickness = (preview.diameter.max(1) as f32 * zoom as f32).max(1.0);
    match preview.kind {
        PreviewKind::Line => canvas.draw_line(from, to, color, thickness),
        PreviewKind::Rectangle => {
            let rect = Rect::new(
                from.x.min(to.x),
                from.y.min(to.y),
                from.x.max(to.x).max(from.x.min(to.x) + 1),
                from.y.max(to.y).max(from.y.min(to.y) + 1),
            );
            canvas.stroke_rect(rect, color, thickness);
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
                thickness,
            );
        }
    }
}
