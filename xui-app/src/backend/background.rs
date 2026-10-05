//! The window background under every repaint: the theme's vertical gradient
//! and the backdrop picture over it, rendered once for the window's size and
//! copied into each damaged rectangle.
//!
//! Rasterizing the gradient costs tens of nanoseconds a pixel, and a page view
//! or a canvas repaints its whole area every frame (LazyWeb scrolling), so a
//! fresh gradient under it took as long as the page itself. The cached picture
//! is opaque and drawn 1:1, which the canvas does as a plain row copy.

use xui_canvas::Surface;
use xui_core::backend::Canvas;
use xui_core::image::Image;
use xui_core::theme::look;
use xui_core::{Rect, Theme};

use super::backdrop;

/// What the cached picture depends on.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    width: i32,
    height: i32,
    dpi: u32,
    theme: Theme,
    /// The backdrop's identity, if any.
    backdrop: Option<u64>,
}

/// A window's background, rendered for one [`Key`].
#[derive(Default)]
pub(super) struct Background {
    cached: Option<(Key, Image)>,
}

impl Background {
    /// Paints the background into `damage` (window-absolute) of a window of
    /// `width` x `height`, re-rendering the cached picture first when what it
    /// depends on changed.
    pub(super) fn paint(
        &mut self,
        canvas: &mut dyn Canvas,
        damage: Rect,
        (width, height): (i32, i32),
        theme: &Theme,
        picture: Option<&Image>,
    ) {
        let window = Rect::new(0, 0, width, height);
        let key = Key {
            width,
            height,
            dpi: canvas.dpi(),
            theme: *theme,
            backdrop: picture.map(Image::id),
        };
        if self.cached.as_ref().map(|(k, _)| *k) != Some(key) {
            self.cached = render(key, theme, picture).map(|image| (key, image));
        }
        match &self.cached {
            Some((_, image)) => backdrop::draw(canvas, image, window, damage),
            // No picture (a zero-sized window): draw directly, as before.
            None => {
                look::paint_background(canvas, damage, window, theme);
                if let Some(picture) = picture {
                    backdrop::draw(canvas, picture, window, damage);
                }
            }
        }
    }
}

/// The whole window's background as one opaque picture.
fn render(key: Key, theme: &Theme, picture: Option<&Image>) -> Option<Image> {
    let (width, height) = (
        u32::try_from(key.width).ok()?,
        u32::try_from(key.height).ok()?,
    );
    if width == 0 || height == 0 {
        return None;
    }
    let window = Rect::new(0, 0, key.width, key.height);
    let mut surface = Surface::new(width, height);
    surface.with_canvas_at(window, key.dpi, |canvas| {
        look::paint_background(canvas, window, window, theme);
        if let Some(picture) = picture {
            backdrop::draw(canvas, picture, window, window);
        }
    });
    let mut pixels = surface.pixels().to_vec();
    // The theme's colours are opaque; make sure of it, so the copy stays a
    // plain copy and never blends with the previous frame.
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    Image::from_rgba(width, height, pixels).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painted(background: &mut Background, theme: &Theme, damage: Rect) -> Surface {
        let mut surface = Surface::new(8, 8);
        surface.with_canvas_at(damage, 96, |canvas| {
            background.paint(canvas, damage, (8, 8), theme, None)
        });
        surface
    }

    fn reference(theme: &Theme, damage: Rect) -> Surface {
        let mut surface = Surface::new(8, 8);
        surface.with_canvas_at(damage, 96, |canvas| {
            look::paint_background(canvas, damage, Rect::new(0, 0, 8, 8), theme)
        });
        surface
    }

    fn gradient() -> Theme {
        let mut theme = Theme::light();
        theme.background_end = xui_core::Color::rgb(0, 0, 0);
        theme
    }

    #[test]
    fn the_damage_matches_a_direct_paint_and_nothing_else_is_touched() {
        let theme = gradient();
        let damage = Rect::new(2, 1, 6, 7);
        let mut background = Background::default();
        let cached = painted(&mut background, &theme, damage);
        let direct = reference(&theme, damage);
        assert_eq!(cached.pixels(), direct.pixels());
        // Outside the damage the surface stays transparent.
        assert_eq!(&cached.pixels()[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn a_new_theme_renders_again() {
        let mut background = Background::default();
        let full = Rect::new(0, 0, 8, 8);
        let first = painted(&mut background, &gradient(), full);
        let dark = Theme::dark();
        let second = painted(&mut background, &dark, full);
        assert_ne!(first.pixels(), second.pixels());
        assert_eq!(second.pixels(), reference(&dark, full).pixels());
    }
}
