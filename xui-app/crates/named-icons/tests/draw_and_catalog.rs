//! `draw` paints a known name and nothing for an unknown one, and the
//! checked-in `docs/icons.md` is exactly what the generator writes.

use std::path::Path;
use xui_canvas::Surface;
use xui_core::{Color, Rect};

const SIDE: u32 = 32;

/// The premultiplied pixels after drawing `name` on a transparent square,
/// and whether `draw` reported it known.
fn render(name: &str) -> (Vec<u8>, bool) {
    let rect = Rect::new(0, 0, SIDE as i32, SIDE as i32);
    let mut surface = Surface::new(SIDE, SIDE);
    let mut drawn = false;
    surface.with_canvas(rect, |canvas| {
        drawn = lazyicons::draw(canvas, name, rect, Color::rgb(0, 0, 0), 96);
    });
    (surface.pixels().to_vec(), drawn)
}

fn painted(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] != 0)
        .count()
}

#[test]
fn a_known_name_paints() {
    for name in ["save", "volume-2", "x"] {
        let (pixels, drawn) = render(name);
        assert!(drawn, "{name}");
        assert!(painted(&pixels) > 0, "{name} left the canvas blank");
    }
}

#[test]
fn an_unknown_name_paints_nothing() {
    for name in ["", "Save", "no-such-icon"] {
        let (pixels, drawn) = render(name);
        assert!(!drawn, "{name:?}");
        assert_eq!(painted(&pixels), 0, "{name:?} painted pixels");
    }
}

#[test]
fn checked_in_catalog_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/icons.md");
    let current = std::fs::read_to_string(&path).expect("docs/icons.md exists");
    // A Windows checkout may hold CRLF; compare the content.
    assert!(
        current.replace("\r\n", "\n") == lazyicons::catalog_markdown(),
        "docs/icons.md is stale: run `cargo run -p lazyicons --example catalog` in xui-app/"
    );
}

#[test]
fn catalog_lists_every_name() {
    let text = lazyicons::catalog_markdown();
    for (name, _) in lazyicons::all() {
        assert!(text.contains(&format!("| `{name}` |")), "{name} missing");
    }
}
