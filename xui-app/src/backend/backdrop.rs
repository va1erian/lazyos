//! A window's backdrop: a picture under every node, over the theme's
//! background (LazyShell's wallpaper).
//!
//! The compositor paints it right after the window background, and widgets
//! draw on what is under them (`Canvas::composites_parents`) instead of
//! filling their own background, so the desktop's icon view sits on the
//! picture.

use std::rc::Rc;
use std::sync::atomic::Ordering;

use xui_core::backend::{Canvas, WindowId};
use xui_core::image::Image;
use xui_core::Rect;

use super::LazyOSBackend;

impl LazyOSBackend {
    /// Show `image` under `window`'s nodes (`None`: the theme background
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

#[cfg(test)]
mod tests {
    use xui_canvas::Surface;

    use super::*;

    fn pixel(surface: &Surface, x: usize, y: usize) -> [u8; 3] {
        let at = (y * 8 + x) * 4;
        let p = &surface.pixels()[at..at + 4];
        [p[0], p[1], p[2]]
    }

    #[test]
    fn the_picture_covers_the_window_but_only_the_damage_is_drawn() {
        let image = Image::from_rgba(4, 4, [255, 0, 0, 255].repeat(16)).expect("image");
        let mut surface = Surface::new(8, 8);
        let (window, damage) = (Rect::new(0, 0, 8, 8), Rect::new(2, 2, 6, 6));
        surface.with_canvas_at(damage, 96, |canvas| draw(canvas, &image, window, damage));
        assert_eq!(pixel(&surface, 2, 2), [255, 0, 0]);
        assert_eq!(pixel(&surface, 5, 5), [255, 0, 0]);
        assert_ne!(pixel(&surface, 0, 0), [255, 0, 0], "outside the damage");
        assert_ne!(pixel(&surface, 7, 3), [255, 0, 0], "outside the damage");
    }
}
