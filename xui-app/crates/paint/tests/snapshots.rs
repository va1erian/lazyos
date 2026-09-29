//! Headless snapshot tests: light and dark, 96 and 192 DPI, at two design
//! sizes. Pixel predicates only, no golden files. Also writes PNGs to
//! `target/snapshots/` for eyeballing.

use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, try_render};
use xui_core::Dip;
use xui_core::Theme;
use xui_core::image::Image;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{PaintApp, layout};

const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Renders the app at a design size, theme and DPI.
fn render(width: f32, height: f32, theme: Theme, dpi: u32) -> Image {
    try_render(
        Snapshot::new(Dip(width), Dip(height)).theme(theme).dpi(dpi),
        |ui| PaintApp::build(ui, Rc::new(MemoryStorage::new())),
    )
    .expect("render xpaint")
}

/// Whether any pixel inside `rect` is white.
fn has_white(image: &Image, rect: (i32, i32, i32, i32)) -> bool {
    let (left, top, right, bottom) = rect;
    (top.max(0)..bottom.min(image.height() as i32)).any(|y| {
        (left.max(0)..right.min(image.width() as i32))
            .any(|x| image.pixel(x as u32, y as u32) == Some(WHITE))
    })
}

/// Whether every sampled pixel in `rect` is not white.
fn no_white(image: &Image, rect: (i32, i32, i32, i32)) -> bool {
    let (left, top, right, bottom) = rect;
    (top.max(0)..bottom.min(image.height() as i32)).all(|y| {
        (left.max(0)..right.min(image.width() as i32))
            .all(|x| image.pixel(x as u32, y as u32) != Some(WHITE))
    })
}

/// Saves a rendered image for eyeballing.
fn save(name: &str, image: &Image) {
    std::fs::create_dir_all("target/snapshots").unwrap();
    image
        .save_png(format!("target/snapshots/{name}.png"))
        .expect("save snapshot");
}

fn check(width: f32, height: f32, dpi: u32) {
    let scale = dpi as f32 / 96.0;
    let client = xui_core::Rect::new(0, 0, (width * scale) as i32, (height * scale) as i32);
    let areas = layout(client, dpi, true);
    let light = render(width, height, Theme::light(), dpi);
    let dark = render(width, height, Theme::dark(), dpi);

    let name = format!("xpaint-{}x{}-{dpi}dpi", width as i32, height as i32);
    save(&format!("{name}-light"), &light);
    save(&format!("{name}-dark"), &dark);

    // The bitmap (320x240 canvas pixels) is white at rest; sample well inside
    // it, clear of the viewport border.
    let sample = (areas.canvas.left + 50, areas.canvas.top + 50);
    assert_eq!(
        light.pixel(sample.0 as u32, sample.1 as u32),
        Some(WHITE),
        "{name}: the canvas did not paint white"
    );
    assert_eq!(
        dark.pixel(sample.0 as u32, sample.1 as u32),
        Some(WHITE),
        "{name}: the dark canvas did not paint white"
    );

    // The chrome is painted (not white) and does not leak the canvas white.
    assert!(
        has_white(
            &light,
            (
                areas.canvas.left,
                areas.canvas.top,
                areas.canvas.right,
                areas.canvas.bottom
            )
        ),
        "{name}: the canvas is white"
    );
    assert!(
        no_white(
            &light,
            (
                areas.status.left,
                areas.status.top,
                areas.status.right,
                areas.status.bottom
            )
        ),
        "{name}: the canvas leaked into the status bar"
    );
    assert!(
        no_white(
            &light,
            (
                areas.toolbar.left,
                areas.toolbar.top,
                areas.toolbar.right,
                areas.toolbar.bottom
            )
        ),
        "{name}: the canvas leaked into the toolbar"
    );

    // Light and dark differ in the chrome outside the canvas.
    let differs = (areas.status.top..areas.status.bottom.min(light.height() as i32)).any(|y| {
        (areas.status.left..areas.status.right.min(light.width() as i32))
            .any(|x| light.pixel(x as u32, y as u32) != dark.pixel(x as u32, y as u32))
    });
    assert!(differs, "{name}: light and dark chrome are identical");
}

#[test]
fn snapshots_at_96_dpi() {
    check(320.0, 240.0, 96);
    check(800.0, 600.0, 96);
}

#[test]
fn snapshots_at_192_dpi() {
    check(320.0, 240.0, 192);
    check(800.0, 600.0, 192);
}
