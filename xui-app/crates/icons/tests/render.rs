//! Every icon renders on the tiny-skia software canvas: it leaves opaque ink
//! and colour, keeps the surface's transparent corners, and a retinted palette
//! changes the pixels.

use xui_canvas::{RgbaImage, Surface};
use xui_core::Rect;
use xui_core::backend::Rgba;
use xui_icons::{Palette, Tone, Village, draw};

fn render(icon: Village, side: i32, palette: &Palette) -> RgbaImage {
    let bounds = Rect::new(0, 0, side, side);
    let mut surface = Surface::new(side as u32, side as u32);
    surface.with_canvas(bounds, |canvas| draw(canvas, icon, bounds, palette));
    surface.to_image()
}

fn opaque(image: &RgbaImage) -> usize {
    image.pixels.chunks(4).filter(|p| p[3] == 0xFF).count()
}

#[test]
fn every_icon_draws_at_common_sizes() {
    for &icon in Village::ALL {
        for side in [16, 24, 32, 64] {
            let image = render(icon, side, &Palette::GLOBAL_VILLAGE);
            assert!(
                opaque(&image) > (side * side) as usize / 40,
                "{icon:?} is nearly empty at {side}px"
            );
        }
    }
}

#[test]
fn the_background_stays_transparent() {
    // No icon paints the very corner of its box: the alpha channel is real.
    for &icon in Village::ALL {
        let image = render(icon, 64, &Palette::GLOBAL_VILLAGE);
        assert_eq!(image.pixel(0, 0).unwrap()[3], 0, "{icon:?}");
    }
}

#[test]
fn a_palette_retints_the_render() {
    let teal = render(Village::Info, 64, &Palette::GLOBAL_VILLAGE);
    let red = Palette::GLOBAL_VILLAGE.with(Tone::Teal, Rgba::rgb(0xFF, 0x00, 0x00));
    assert_ne!(teal.pixels, render(Village::Info, 64, &red).pixels);
}

#[test]
fn an_empty_rect_draws_nothing() {
    let bounds = Rect::new(0, 0, 8, 8);
    let mut surface = Surface::new(8, 8);
    surface.with_canvas(bounds, |canvas| {
        draw(
            canvas,
            Village::Home,
            Rect::new(4, 4, 4, 4),
            &Palette::GLOBAL_VILLAGE,
        );
    });
    assert_eq!(opaque(&surface.to_image()), 0);
}
