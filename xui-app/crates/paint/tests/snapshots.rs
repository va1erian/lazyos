//! Headless snapshot tests: light and dark, 96 and 192 DPI, at two design
//! sizes. Pixel predicates only, no golden files. Also writes PNGs to
//! `target/snapshots/` for eyeballing.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::Dip;
use xui_core::Theme;
use xui_core::image::Image;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::PaintApp;

use common::{Areas, Widgets};

const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Renders the app at a design size, theme and DPI, with where the layout
/// placed its widgets.
fn render(width: f32, height: f32, theme: Theme, dpi: u32) -> (Image, Areas) {
    let widgets = Widgets::default();
    let built = widgets.clone();
    let areas = Rc::new(Cell::new(None));
    let seen = Rc::clone(&areas);
    let image = render_with(
        Snapshot::new(Dip(width), Dip(height)).theme(theme).dpi(dpi),
        move |ui| built.record(PaintApp::build(ui, Rc::new(MemoryStorage::new()))),
        move |stage| seen.set(Some(widgets.areas(stage.ui()))),
    )
    .expect("render xpaint");
    (image, areas.get().expect("the step ran"))
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
    let (light, areas) = render(width, height, Theme::light(), dpi);
    let (dark, _) = render(width, height, Theme::dark(), dpi);

    let name = format!("xpaint-{}x{}-{dpi}dpi", width as i32, height as i32);
    save(&format!("{name}-light"), &light);
    save(&format!("{name}-dark"), &dark);

    // The bitmap (320x240 canvas pixels) is white at rest; sample well inside
    // it, clear of the viewport border.
    let sample = areas.pixel(50, 50);
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

/// Renders the app after `msgs`, at `dpi` in `theme`, and saves the PNG as
/// `target/snapshots/<name>.png`.
fn render_after(name: &str, msgs: Vec<xui_paint::Msg>, theme: Theme, dpi: u32) -> Image {
    let image = render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)).theme(theme).dpi(dpi),
        |ui| PaintApp::build(ui, Rc::new(MemoryStorage::new())),
        move |stage| {
            for msg in msgs {
                stage.emit(msg);
            }
        },
    )
    .expect("render xpaint");
    save(name, &image);
    image
}

#[test]
fn the_options_bar_shows_the_size_for_a_brush_and_hides_it_for_a_pencil() {
    use xui_paint::{Msg, Tool};
    for (theme, label) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for dpi in [96, 192] {
            let scale = dpi as i32 / 96;
            let pencil = render_after(
                "xpaint-pencil-options",
                vec![Msg::Tool(Tool::Pencil)],
                theme,
                dpi,
            );
            let brush = render_after(
                &format!("xpaint-brush-options-{dpi}dpi-{label}"),
                vec![Msg::Tool(Tool::Brush), Msg::Size(8)],
                theme,
                dpi,
            );
            // The size box sits right of the tool name, in the options bar
            // (design y 30..66): it differs between the two tools there.
            let differs = (150 * scale..260 * scale).any(|x| {
                (36 * scale..60 * scale)
                    .any(|y| pencil.pixel(x as u32, y as u32) != brush.pixel(x as u32, y as u32))
            });
            assert!(differs, "{label} {dpi}: the Brush shows a size control");
        }
    }
}

#[test]
fn the_edit_menu_opens_over_the_canvas() {
    use xui_core::message::{Modifiers, MouseButton};
    for (theme, label) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        let closed = render_after("xpaint-menu-closed", vec![], theme, 96);
        let open = render_with(
            Snapshot::new(Dip(800.0), Dip(600.0)).theme(theme),
            |ui| PaintApp::build(ui, Rc::new(MemoryStorage::new())),
            |stage| {
                // The "Edit" title sits at about x = 75 in the 30 px menu bar.
                let (x, y) = (75, 14);
                for event in [
                    xui_core::backend::Event::MouseDown {
                        x,
                        y,
                        button: MouseButton::Left,
                        modifiers: Modifiers::NONE,
                    },
                    xui_core::backend::Event::MouseUp {
                        x,
                        y,
                        button: MouseButton::Left,
                        modifiers: Modifiers::NONE,
                    },
                ] {
                    stage.inject(event);
                }
            },
        )
        .expect("render xpaint");
        save(&format!("xpaint-edit-menu-{label}"), &open);
        // The popup drops below the bar over the options bar and the canvas.
        let differs = (40..300).any(|y| (60..260).any(|x| open.pixel(x, y) != closed.pixel(x, y)));
        assert!(differs, "{label}: the menu drew nothing");
    }
}

#[test]
fn the_colour_editor_snapshots_replace_the_canvas() {
    use xui_paint::Msg;
    use xui_paint::model::Side;
    for (theme, label) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        for dpi in [96, 192] {
            let scale = dpi as i32 / 96;
            let open = render_after(
                &format!("xpaint-colour-editor-{dpi}dpi-{label}"),
                vec![Msg::EditColor(Side::Primary)],
                theme,
                dpi,
            );
            // The picture (320x240 px, centred in the canvas) is gone: some pixel
            // of its area is not the bitmap white (the editor's own chrome).
            let (x, y) = ((356 * scale - 150) as u32, (321 * scale + 100) as u32);
            let hidden = (0..40).any(|d| open.pixel(x + d * 3, y - d * 4) != Some(WHITE));
            assert!(
                hidden,
                "{label} {dpi}: the canvas still shows under the editor"
            );
            // Cancel puts the canvas back.
            let closed = render_after(
                "xpaint-colour-editor-closed",
                vec![Msg::EditColor(Side::Primary), Msg::ColorCancel],
                theme,
                dpi,
            );
            assert_eq!(
                closed.pixel(x, y),
                Some(WHITE),
                "{label} {dpi}: the canvas is back"
            );
        }
    }
}

#[test]
fn zooming_in_from_the_view_menu_enlarges_the_picture() {
    use xui_paint::Msg;
    use xui_paint::view::Command;
    for (theme, label) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
        let base = render_after("xpaint-zoom-base", vec![], theme, 96);
        let zoomed = render_after(
            &format!("xpaint-zoom-200-{label}"),
            vec![
                Msg::Command(Command::ZoomToWindow),
                Msg::Command(Command::PixelGrid),
            ],
            theme,
            96,
        );
        // Fit-to-window grows the 320x240 picture past its 100% footprint: the
        // white now reaches below the old bottom edge (canvas top 66 + 201 + 240).
        let below = (320, 470);
        assert_ne!(base.pixel(below.0, below.1), Some(WHITE), "{label}");
        assert_eq!(
            zoomed.pixel(below.0, below.1),
            Some(WHITE),
            "{label}: zoomed"
        );
    }
}
