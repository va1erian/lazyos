//! A window's backdrop: a picture under every node, in place of the flat
//! background colour (LazyShell's wallpaper).
//!
//! xui colours are opaque, so a widget that clears itself to the theme
//! background (the desktop's icon view) would paint a flat block over a
//! picture drawn beneath it. Painters of a window with a backdrop therefore
//! draw through a [`BackdropCanvas`]: clearing to the window background shows
//! the picture instead, and everything else goes straight to the real canvas.

use std::rc::Rc;
use std::sync::atomic::Ordering;

use xui_core::backend::{
    Canvas, Corner, LinearGradient, PathGradient, PathPlacement, PathSeg, RadialGradient, Rgba,
    Stroke, TextLayout, TextMetrics, TextStyle, WindowId,
};
use xui_core::image::Image;
use xui_core::{Color, Point, Rect};

use super::LazyOSBackend;

impl LazyOSBackend {
    /// Show `image` under `window`'s nodes (`None`: the flat background
    /// again). The picture is stretched over the whole window, so hand over
    /// one already fitted to the window's size.
    pub fn set_backdrop(&self, window: WindowId, image: Option<Rc<Image>>) {
        let full = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return;
            };
            entry.backdrop = image;
            Rect::new(0, 0, entry.width, entry.height)
        };
        if self.is_client() {
            self.add_damage(window, full);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }
}

/// Draw `image` over `area` (the window), cut to `visible`.
pub(super) fn draw(canvas: &mut dyn Canvas, image: &Image, area: Rect, visible: Rect) {
    canvas.push_clip(visible);
    canvas.draw_image(image, area);
    canvas.pop_clip();
}

/// A canvas whose `clear` to the window background shows the backdrop.
pub(super) struct BackdropCanvas<'a> {
    pub inner: &'a mut dyn Canvas,
    pub image: &'a Image,
    /// The window, in the canvas's (surface) coordinates.
    pub area: Rect,
    pub background: Color,
}

impl Canvas for BackdropCanvas<'_> {
    fn dpi(&self) -> u32 {
        self.inner.dpi()
    }

    fn bounds(&self) -> Rect {
        self.inner.bounds()
    }

    fn clear(&mut self, color: Color) {
        if color == self.background {
            let visible = self.inner.bounds();
            draw(self.inner, self.image, self.area, visible);
        } else {
            self.inner.clear(color);
        }
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.inner.fill_rect(rect, color);
    }

    fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        self.inner.fill_rounded_rect(rect, radius, color);
    }

    fn fill_ellipse(&mut self, center: Point, radius_x: f32, radius_y: f32, color: Color) {
        self.inner.fill_ellipse(center, radius_x, radius_y, color);
    }

    fn fill_polygon(&mut self, points: &[Point], color: Color) {
        self.inner.fill_polygon(points, color);
    }

    fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        self.inner.stroke_rect(rect, color, width);
    }

    fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color, width: f32) {
        self.inner.stroke_rounded_rect(rect, radius, color, width);
    }

    fn stroke_ellipse(
        &mut self,
        center: Point,
        radius_x: f32,
        radius_y: f32,
        color: Color,
        width: f32,
    ) {
        self.inner
            .stroke_ellipse(center, radius_x, radius_y, color, width);
    }

    fn draw_line(&mut self, from: Point, to: Point, color: Color, width: f32) {
        self.inner.draw_line(from, to, color, width);
    }

    fn fill_rect_rgba(&mut self, rect: Rect, color: Rgba) {
        self.inner.fill_rect_rgba(rect, color);
    }

    fn fill_rounded_rect_corners(&mut self, rect: Rect, corners: [Corner; 4], color: Rgba) {
        self.inner.fill_rounded_rect_corners(rect, corners, color);
    }

    fn stroke_rounded_rect_corners(
        &mut self,
        rect: Rect,
        corners: [Corner; 4],
        color: Rgba,
        stroke: &Stroke,
    ) {
        self.inner
            .stroke_rounded_rect_corners(rect, corners, color, stroke);
    }

    fn draw_line_stroked(&mut self, from: Point, to: Point, color: Rgba, stroke: &Stroke) {
        self.inner.draw_line_stroked(from, to, color, stroke);
    }

    fn stroke_ellipse_stroked(
        &mut self,
        center: Point,
        radius_x: f32,
        radius_y: f32,
        color: Rgba,
        stroke: &Stroke,
    ) {
        self.inner
            .stroke_ellipse_stroked(center, radius_x, radius_y, color, stroke);
    }

    fn fill_path(&mut self, path: &[PathSeg], at: PathPlacement, color: Rgba) {
        self.inner.fill_path(path, at, color);
    }

    fn fill_path_linear(&mut self, path: &[PathSeg], at: PathPlacement, gradient: &PathGradient) {
        self.inner.fill_path_linear(path, at, gradient);
    }

    fn stroke_path(&mut self, path: &[PathSeg], at: PathPlacement, color: Rgba, stroke: &Stroke) {
        self.inner.stroke_path(path, at, color, stroke);
    }

    fn fill_rect_linear(&mut self, rect: Rect, gradient: &LinearGradient) {
        self.inner.fill_rect_linear(rect, gradient);
    }

    fn fill_rect_radial(&mut self, rect: Rect, gradient: &RadialGradient) {
        self.inner.fill_rect_radial(rect, gradient);
    }

    fn draw_text(&mut self, text: &str, rect: Rect, style: &TextStyle) {
        self.inner.draw_text(text, rect, style);
    }

    fn measure_text(&self, text: &str, style: &TextStyle) -> TextMetrics {
        self.inner.measure_text(text, style)
    }

    fn draw_layout(&mut self, layout: &dyn TextLayout, origin: Point, color: Rgba) {
        self.inner.draw_layout(layout, origin, color);
    }

    fn draw_image(&mut self, image: &Image, rect: Rect) {
        self.inner.draw_image(image, rect);
    }

    fn push_clip(&mut self, rect: Rect) {
        self.inner.push_clip(rect);
    }

    fn push_clip_rounded(&mut self, rect: Rect, corners: [Corner; 4]) {
        self.inner.push_clip_rounded(rect, corners);
    }

    fn pop_clip(&mut self) {
        self.inner.pop_clip();
    }

    fn save(&mut self) {
        self.inner.save();
    }

    fn restore(&mut self) {
        self.inner.restore();
    }

    fn set_translation(&mut self, x: f32, y: f32) {
        self.inner.set_translation(x, y);
    }

    fn set_scale_translate(&mut self, scale: f32, x: f32, y: f32) {
        self.inner.set_scale_translate(scale, x, y);
    }
}

#[cfg(test)]
mod tests {
    use xui_canvas::Surface;

    use super::*;

    const BACKGROUND: Color = Color::rgb(10, 20, 30);

    fn red() -> Image {
        Image::from_rgba(4, 4, [255, 0, 0, 255].repeat(16)).expect("image")
    }

    fn pixel(surface: &Surface, x: usize, y: usize) -> [u8; 3] {
        let at = (y * 8 + x) * 4;
        let p = &surface.pixels()[at..at + 4];
        [p[0], p[1], p[2]]
    }

    /// Clear a node at `(2, 2)..(6, 6)` of an 8x8 window to `color`.
    fn cleared(color: Color) -> Surface {
        let image = red();
        let mut surface = Surface::new(8, 8);
        surface.with_canvas_at(Rect::new(2, 2, 6, 6), 96, |canvas| {
            BackdropCanvas {
                inner: canvas,
                image: &image,
                area: Rect::new(0, 0, 8, 8),
                background: BACKGROUND,
            }
            .clear(color);
        });
        surface
    }

    #[test]
    fn clearing_to_the_background_shows_the_picture_inside_the_node_only() {
        let surface = cleared(BACKGROUND);
        assert_eq!(pixel(&surface, 3, 3), [255, 0, 0]);
        assert_eq!(pixel(&surface, 5, 5), [255, 0, 0]);
        assert_ne!(pixel(&surface, 0, 0), [255, 0, 0], "outside the node");
        assert_ne!(pixel(&surface, 7, 3), [255, 0, 0], "outside the node");
    }

    #[test]
    fn clearing_to_another_colour_is_a_plain_clear() {
        let surface = cleared(Color::rgb(0, 0, 255));
        assert_eq!(pixel(&surface, 3, 3), [0, 0, 255]);
    }
}
