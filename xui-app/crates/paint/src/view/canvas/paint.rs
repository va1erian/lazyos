#![forbid(unsafe_code)]

//! The canvas painter: the screen image 1:1, the brush ring, the text caret
//! and block, the Zoom tool's rubber band and the marching ants.
//!
//! Shapes, gradients and text are real pixels in the layer while they are
//! being made (the model repaints them per frame), so nothing here previews
//! them; what is drawn here is never part of the picture.

use xui_core::{Color, Theme};
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};

use super::ants;
use super::state::CanvasState;
use crate::model::{Rect as PixelRect, Tool};

const WHITE: Color = Color::rgb(255, 255, 255);
const BLACK: Color = Color::rgb(0, 0, 0);

/// Ticks of the canvas timer per half period of the caret's blink.
const BLINK_TICKS: u32 = 3;

pub(super) fn paint(canvas: &mut dyn Canvas, state: &mut CanvasState, theme: &Theme) {
    let bounds = canvas.bounds();
    state.set_view(bounds);
    canvas.clear(theme.background);
    canvas.push_clip(bounds);

    // The screen image is already scaled to the zoom and covers only what is
    // visible, so it is drawn 1:1: xui's bilinear filter has nothing to blur.
    if let Some((image, (x, y))) = state.screen() {
        let (width, height) = (image.width() as i32, image.height() as i32);
        let (left, top) = (bounds.left + x, bounds.top + y);
        canvas.draw_image(image, Rect::new(left, top, left + width, top + height));
    }
    let zoom = state.viewport().zoom();
    // The view position of the middle of picture pixel `point`.
    let at = |point: (i32, i32)| {
        let (x, y) = state.viewport().to_view(point.0, point.1);
        let half = (zoom / 2.0).round() as i32;
        Point::new(bounds.left + x + half, bounds.top + y + half)
    };
    if let Some(cursor) = state.cursor
        && matches!(state.tool, Tool::Pencil | Tool::Brush | Tool::Eraser)
    {
        // xui gap: G4 — `Cursor` has no crosshair and the offscreen backend
        // ignores cursors, so the canvas paints its own brush ring.
        let width = if state.tool == Tool::Pencil { 1 } else { state.brush };
        let radius = (width.max(1) as f32 * zoom as f32 / 2.0).max(1.0);
        canvas.stroke_ellipse(at(cursor), radius, radius, theme.text, 1.0);
    }
    draw_text_overlay(canvas, state, theme, bounds);
    if let Some((a, b)) = state.marquee {
        let rect = corner_box(state, bounds, PixelRect::from_points(a, b));
        canvas.stroke_rect(rect, theme.text, 1.0);
    }
    ants::draw(canvas, state, bounds);
    canvas.stroke_rect(bounds, theme.border, 1.0);
    canvas.pop_clip();
}

/// A picture rectangle in view coordinates, placed in the node.
fn corner_box(state: &CanvasState, bounds: Rect, rect: PixelRect) -> Rect {
    let vp = state.viewport();
    let (x0, y0) = vp.to_view(rect.x0, rect.y0);
    let (x1, y1) = vp.to_view(rect.x1, rect.y1);
    Rect::new(
        bounds.left + x0,
        bounds.top + y0,
        bounds.left + x1.max(x0 + 1),
        bounds.top + y1.max(y0 + 1),
    )
}

/// The text block's outline, its drag handle and the blinking caret.
fn draw_text_overlay(canvas: &mut dyn Canvas, state: &CanvasState, theme: &Theme, bounds: Rect) {
    if !state.editing {
        return;
    }
    if let Some((block, handle)) = state.block {
        canvas.stroke_rect(corner_box(state, bounds, block), theme.border, 1.0);
        let handle = corner_box(state, bounds, handle);
        canvas.fill_rect(handle, theme.accent);
        canvas.stroke_rect(handle, theme.border, 1.0);
    }
    let visible = (state.phase / BLINK_TICKS).is_multiple_of(2);
    if let (true, Some((x, y, height))) = (visible, state.caret) {
        let zoom = state.viewport().zoom() as f32;
        let (ox, oy) = state.viewport().origin();
        let left = bounds.left + (x * zoom).round() as i32 - ox;
        let top = bounds.top + (y * zoom).round() as i32 - oy;
        let bottom = top + (height * zoom).round().max(2.0) as i32;
        // Black with a white edge, so it shows on any picture.
        canvas.fill_rect(Rect::new(left - 1, top - 1, left + 3, bottom + 1), WHITE);
        canvas.fill_rect(Rect::new(left, top, left + 2, bottom), BLACK);
    }
}
