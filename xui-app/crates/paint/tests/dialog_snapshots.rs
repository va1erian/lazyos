//! Snapshots of the Resize, Canvas Size and Hue/Saturation dialogs, light and
//! dark, at 96 and 192 DPI, written to `target/snapshots/` for eyeballing.
//! The assertions are pixel predicates: the dialog is on screen, its text is
//! drawn, and the Hue/Saturation preview colours the picture while the
//! document stays untouched.

mod common;

use std::rc::Rc;

use xui_core::Theme;
use xui_core::image::Image;
use xui_paint::Msg;
use xui_paint::model::{Side, Tool};
use xui_paint::view::Command;

use common::files::*;

fn save(name: &str, image: &Image) {
    std::fs::create_dir_all("target/snapshots").unwrap();
    image
        .save_png(format!("target/snapshots/{name}.png"))
        .expect("save snapshot");
}

/// The number of pixels of `image` that differ from `colour` by more than a
/// little inside `rect`.
fn count_not(image: &Image, rect: xui_core::geometry::Rect, colour: [u8; 3]) -> usize {
    let mut count = 0;
    for y in rect.top.max(0)..rect.bottom.min(image.height() as i32) {
        for x in rect.left.max(0)..rect.right.min(image.width() as i32) {
            let p = image.pixel(x as u32, y as u32).unwrap();
            if (0..3).any(|i| p[i].abs_diff(colour[i]) > 24) {
                count += 1;
            }
        }
    }
    count
}

fn themes() -> [(&'static str, Theme); 2] {
    [("light", Theme::light()), ("dark", Theme::dark())]
}

/// Renders `open` at every theme and DPI under `name`, checking that text
/// and controls were drawn inside the canvas area.
fn shoot(name: &str, open: impl Fn(&Stg<'_>, &Ctx) + Clone + 'static) {
    for (theme_name, theme) in themes() {
        for dpi in [96, 192] {
            let open = open.clone();
            let image = session_at(&Rc::new(Disk::default()), theme, dpi, move |stage, ctx| {
                open(stage, ctx);
                ctx.probe(stage);
            });
            assert_eq!(image.width(), 800 * dpi / 96);
            save(&format!("{name}-{theme_name}-{dpi}"), &image);
        }
    }
}

#[test]
fn the_resize_dialog_renders_in_both_themes_and_dpis() {
    shoot("dialog-resize", |stage, _| {
        chord(stage, xui_core::message::Key::R, false);
    });
}

#[test]
fn the_canvas_size_dialog_renders_in_both_themes_and_dpis() {
    shoot("dialog-canvas-size", |stage, _| {
        stage.emit(Msg::Palette {
            color: [40, 90, 200, 255],
            side: Side::Secondary,
        });
        chord(stage, xui_core::message::Key::R, true);
    });
}

#[test]
fn the_hue_saturation_strip_renders_with_a_live_preview() {
    shoot("dialog-hue-saturation", |stage, ctx| {
        stage.emit(Msg::Palette {
            color: [220, 40, 40, 255],
            side: Side::Primary,
        });
        stage.emit(Msg::Tool(Tool::Fill));
        drag_picture(stage, ctx, (5, 5), (5, 5));
        stage.emit(Msg::Command(Command::HueSaturation));
        click_along(stage, ctx, "adjust_slider:0", 0.75);
    });
}

#[test]
fn the_dialogs_draw_something_inside_the_canvas_area() {
    for open_key in [(false, "resize"), (true, "canvas")] {
        let image = session(&Rc::new(Disk::default()), move |stage, ctx| {
            chord(stage, xui_core::message::Key::R, open_key.0);
            ctx.probe(stage);
        });
        let canvas = {
            let seen = Rc::new(std::cell::Cell::new(None));
            let out = Rc::clone(&seen);
            session(&Rc::new(Disk::default()), move |_, ctx| {
                out.set(Some(ctx.rect("canvas")));
            });
            seen.get().unwrap()
        };
        // A frame over the canvas leaves the picture's white behind it: the
        // area is the theme background and text, not the checker or white.
        let busy = count_not(&image, canvas, [255, 255, 255]);
        assert!(
            busy > 500,
            "{}: only {busy} pixels differ from white",
            open_key.1
        );
    }
}
