//! Every packaged app's icon, drawn from the xui icon sets.
//!
//! A package must carry `icons/app-16.png`, `-32` and `-128`
//! (`docs/packages.md`); the desktop shows the 32-pixel one. They are checked
//! in, and this crate is how they are made: each package names a Global
//! Village picture (`xui-icons`, the set LazyShell draws its built-in launchers
//! with) or, where that set has nothing fitting, a Lucide outline on a tile in
//! the Global Village colours. `cargo run -p app-icons` rewrites them all.

use xui_canvas::Surface;
use xui_core::backend::{Canvas, Rgba};
use xui_core::image::Image;
use xui_core::{draw_icon, Color, Lucide, Rect};
use xui_icons::{Icon, Palette, Tone};

/// The side lengths every package ships, in pixels.
pub const SIZES: [u32; 3] = [16, 32, 128];

/// What one package's icon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Art {
    /// A Global Village picture, in its own colours.
    Village(Icon),
    /// A Lucide outline in cream on an ink-edged tile of `Tone`.
    Lucide(Lucide, Tone),
}

/// Each package's directory (relative to the repository root) and its art.
pub const PACKAGES: &[(&str, Art)] = &[
    ("xui-app/packages/confd", Art::Village(Icon::Server)),
    (
        "xui-app/packages/counter",
        Art::Lucide(Lucide::Plus, Tone::Teal),
    ),
    ("xui-app/packages/docs", Art::Village(Icon::Help)),
    ("xui-app/packages/editor", Art::Village(Icon::Document)),
    ("xui-app/packages/fabricmon", Art::Village(Icon::PubSub)),
    ("xui-app/packages/files", Art::Village(Icon::Folder)),
    // LazyRAD (#532): a window on a teal tile, the form designer's look. The
    // checked-in PNGs are the package's own teal placeholders, drawn before
    // this entry; `cargo run -p app-icons` redraws them from it.
    (
        "xui-app/packages/lazyrad",
        Art::Lucide(Lucide::AppWindow, Tone::Teal),
    ),
    // LazyWeb: neither set has a globe; a link on an amber tile.
    (
        "xui-app/packages/lazyweb",
        Art::Lucide(Lucide::Link, Tone::Amber),
    ),
    ("xui-app/packages/network", Art::Village(Icon::Network)),
    ("xui-app/packages/nettools", Art::Village(Icon::Modem)),
    ("xui-app/packages/paint", Art::Village(Icon::Image)),
    ("xui-app/packages/settings", Art::Village(Icon::Settings)),
    ("xui-app/packages/sysmon", Art::Village(Icon::Monitor)),
    ("xui-app/packages/widget", Art::Village(Icon::Widget)),
    // LazyWriter: Lucide `file-text`, the outline its own toolbar uses for a
    // document (needs the xui pin with the formatting icons, issue #533).
    (
        "xui-app/packages/writer",
        Art::Lucide(Lucide::FileText, Tone::Cobalt),
    ),
    // The user-package copy of the Counter (`org.lazy.counter`).
    (
        "tools/pkg/samples/counter",
        Art::Lucide(Lucide::Plus, Tone::Teal),
    ),
    ("doom/package", Art::Lucide(Lucide::Zap, Tone::Clay)),
];

/// `art` drawn on a transparent `size` x `size` square.
pub fn render(art: Art, size: u32) -> Image {
    let side = size as i32;
    let rect = Rect::new(0, 0, side, side);
    let mut surface = Surface::new(size, size);
    surface.with_canvas(rect, |canvas| match art {
        Art::Village(icon) => xui_icons::draw(canvas, icon, rect, &Palette::GLOBAL_VILLAGE),
        Art::Lucide(outline, tone) => {
            let palette = Palette::GLOBAL_VILLAGE;
            // Inset by half the edge so the stroke stays inside the square.
            let edge = (side as f32 / 16.0).max(1.0);
            let half = (edge / 2.0).ceil() as i32;
            let tile = Rect::new(half, half, side - half, side - half);
            let radius = side as f32 / 5.0;
            canvas.fill_rounded_rect(tile, radius, color(palette.get(tone)));
            canvas.stroke_rounded_rect(tile, radius, color(palette.get(Tone::Ink)), edge);
            let pad = side / 5;
            let glyph = Rect::new(pad, pad, side - pad, side - pad);
            draw_icon(canvas, outline, glyph, color(palette.get(Tone::Chalk)), 96);
        }
    });
    let pixels = unpremultiply(surface.pixels());
    Image::from_rgba(size, size, pixels).expect("a surface's pixels match its size")
}

/// The opaque colour of a palette entry (every tone used here is opaque).
fn color(rgba: Rgba) -> Color {
    Color::rgb(rgba.r, rgba.g, rgba.b)
}

/// The canvas keeps premultiplied RGBA; PNG stores straight alpha, so the
/// anti-aliased edges would otherwise come out too dark.
fn unpremultiply(premultiplied: &[u8]) -> Vec<u8> {
    let mut pixels = premultiplied.to_vec();
    for px in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(px[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut px[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn repo_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
    }

    #[test]
    fn every_package_manifest_has_art() {
        let packages = repo_root().join("xui-app/packages");
        for entry in std::fs::read_dir(&packages).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let dir = format!("xui-app/packages/{name}");
            assert!(
                PACKAGES.iter().any(|(known, _)| *known == dir),
                "{dir} has no entry in PACKAGES"
            );
        }
        for (dir, _) in PACKAGES {
            assert!(
                repo_root().join(dir).join("manifest.toml").is_file(),
                "{dir}"
            );
        }
    }

    #[test]
    fn every_icon_is_drawn_at_every_size() {
        for &(dir, art) in PACKAGES {
            for size in SIZES {
                let image = render(art, size);
                let pixels = image.pixels();
                let alpha = || pixels.chunks_exact(4).map(|px| px[3]);
                let area = (size * size) as usize;
                let inked = alpha().filter(|&a| a >= 128).count();
                assert!(inked * 8 > area, "{dir} at {size}px is mostly empty");
                assert!(
                    alpha().any(|a| a == 0),
                    "{dir} at {size}px has no transparent edge"
                );
            }
        }
    }

    #[test]
    fn unpremultiply_restores_straight_alpha() {
        assert_eq!(unpremultiply(&[64, 32, 0, 128]), vec![128, 64, 0, 128]);
        assert_eq!(unpremultiply(&[0, 0, 0, 0]), vec![0, 0, 0, 0]);
        assert_eq!(unpremultiply(&[9, 8, 7, 255]), vec![9, 8, 7, 255]);
    }
}
