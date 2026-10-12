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

/// The number of pixels inside `rect` where the two images differ by more than
/// a little.
fn count_diff(a: &Image, b: &Image, rect: xui_core::geometry::Rect) -> usize {
    let mut count = 0;
    for y in rect.top.max(0)..rect.bottom.min(a.height() as i32) {
        for x in rect.left.max(0)..rect.right.min(a.width() as i32) {
            let (p, q) = (a.pixel(x as u32, y as u32), b.pixel(x as u32, y as u32));
            let (p, q) = (p.unwrap(), q.unwrap());
            if (0..3).any(|i| p[i].abs_diff(q[i]) > 24) {
                count += 1;
            }
        }
    }
    count
}

/// The canvas rectangle of a fresh window.
fn canvas_rect() -> xui_core::geometry::Rect {
    let seen = Rc::new(std::cell::Cell::new(None));
    let out = Rc::clone(&seen);
    session(&Rc::new(Disk::default()), move |_, ctx| {
        out.set(Some(ctx.rect("canvas")));
    });
    seen.get().unwrap()
}

#[test]
fn the_dialogs_draw_something_inside_the_canvas_area() {
    let canvas = canvas_rect();
    // The same window with no dialog open, to compare against: the area
    // around the centred picture is the theme background either way.
    let bare = session(&Rc::new(Disk::default()), |_, _| {});
    for open_key in [(false, "resize"), (true, "canvas")] {
        let image = session(&Rc::new(Disk::default()), move |stage, ctx| {
            chord(stage, xui_core::message::Key::R, open_key.0);
            ctx.probe(stage);
        });
        let changed = count_diff(&image, &bare, canvas);
        assert!(
            changed > 500,
            "{}: only {changed} canvas pixels differ from no dialog",
            open_key.1
        );
    }
}

#[test]
fn the_hue_saturation_preview_colours_the_picture_without_a_history_step() {
    let canvas = canvas_rect();
    let state = Rc::new(std::cell::Cell::new((0, 0)));
    let out = Rc::clone(&state);
    let image = session(&Rc::new(Disk::default()), move |stage, ctx| {
        stage.emit(Msg::Palette {
            color: [220, 40, 40, 255],
            side: Side::Primary,
        });
        stage.emit(Msg::Tool(Tool::Fill));
        drag_picture(stage, ctx, (5, 5), (5, 5));
        out.set(ctx.history());
        stage.emit(Msg::Command(Command::HueSaturation));
        click_along(stage, ctx, "adjust_slider:0", 0.75);
        assert_eq!(ctx.history(), out.get(), "the preview is not a step");
        assert!(ctx.dirty(), "the fill is");
    });
    assert_eq!(state.get().0, 1, "only the fill is in the history");
    // The middle of the picture (centred in the canvas) is no longer red.
    let (x, y) = (
        (canvas.left + canvas.right) / 2,
        (canvas.top + canvas.bottom) / 2 - 20,
    );
    let pixel = image.pixel(x as u32, y as u32).unwrap();
    assert!(
        (0..3).any(|i| pixel[i].abs_diff([220, 40, 40][i]) > 40),
        "the preview changed the colour: {pixel:?}"
    );
    assert!(pixel != [255, 255, 255, 255], "and the fill is there");
}
