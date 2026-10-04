//! Design pixels for code laid out in raw pixels (docs/hidpi-plan.md).
//!
//! A window runs at `96 * scale` DPI, so `Dip` sizes (text, xui widgets) are
//! already drawn at the desktop's scale. The dashboards and a few custom
//! painters place things with pixel constants instead. They run unchanged in
//! *design pixels* by switching the canvas to a `scale` transform
//! ([`design_bounds`]): rectangles, strokes and radii are scaled by the
//! canvas, while text keeps its DPI-derived size, so nothing is drawn twice
//! as large and every edge stays crisp. Their pointer hit-tests take
//! [`design_rect`] and [`design_point`].

use std::cell::Cell;

use xui_core::app::Ui;
use xui_core::{Canvas, Rect};

thread_local! {
    /// The UI scale widget layouts written in pixel constants multiply by
    /// ([`rect`]); the backend sets it once it knows the desktop's scale.
    static LAYOUT_SCALE: Cell<i32> = const { Cell::new(1) };
}

/// Set the scale [`rect`] applies (the backend does, on connect).
pub fn set_layout_scale(scale: i32) {
    LAYOUT_SCALE.with(|cell| cell.set(scale.max(1)));
}

/// The scale [`rect`] applies.
pub fn layout_scale() -> i32 {
    LAYOUT_SCALE.with(Cell::get)
}

/// A widget rectangle at `(x, y)` of `w` x `h` design pixels, in screen
/// pixels: what an app laid out in pixel constants passes to a widget.
pub fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    let s = layout_scale();
    Rect::new(x * s, y * s, (x + w) * s, (y + h) * s)
}

/// `rect` (design pixels, left/top/right/bottom) in screen pixels.
pub fn scaled(rect: Rect) -> Rect {
    let s = layout_scale();
    Rect::new(rect.left * s, rect.top * s, rect.right * s, rect.bottom * s)
}

/// The integer UI scale a `dpi` stands for.
pub fn scale_of(dpi: u32) -> i32 {
    (dpi / uitheme::BASE_DPI).max(1) as i32
}

/// `rect` (screen pixels) in design pixels at `scale`.
pub fn down(rect: Rect, scale: i32) -> Rect {
    Rect::new(
        rect.left.div_euclid(scale),
        rect.top.div_euclid(scale),
        rect.right.div_euclid(scale),
        rect.bottom.div_euclid(scale),
    )
}

/// Switch `canvas` to design pixels and return its bounds in them. Call it
/// first in a painter written in pixel constants.
pub fn design_bounds(canvas: &mut dyn Canvas) -> Rect {
    let scale = scale_of(canvas.dpi());
    if scale > 1 {
        canvas.set_scale_translate(scale as f32, 0.0, 0.0);
    }
    down(canvas.bounds(), scale)
}

/// The window's client area in design pixels.
pub fn design_rect<M: 'static>(ui: &Ui<M>) -> Rect {
    down(ui.client_rect(), scale_of(ui.dpi()))
}

/// A pointer position (screen pixels, as events carry it) in design pixels.
pub fn design_point<M: 'static>(ui: &Ui<M>, x: i32, y: i32) -> (i32, i32) {
    let scale = scale_of(ui.dpi());
    (x.div_euclid(scale), y.div_euclid(scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rects_follow_the_layout_scale() {
        assert_eq!(rect(1, 2, 3, 4), Rect::new(1, 2, 4, 6));
        set_layout_scale(2);
        assert_eq!(rect(1, 2, 3, 4), Rect::new(2, 4, 8, 12));
        set_layout_scale(0);
        assert_eq!(layout_scale(), 1);
    }

    #[test]
    fn scale_follows_the_dpi() {
        assert_eq!(scale_of(96), 1);
        assert_eq!(scale_of(192), 2);
        assert_eq!(scale_of(0), 1);
        assert_eq!(
            down(Rect::new(10, 20, 2560, 1440), 2),
            Rect::new(5, 10, 1280, 720)
        );
        assert_eq!(down(Rect::new(-3, 0, 1, 1), 2), Rect::new(-2, 0, 0, 0));
    }
}
