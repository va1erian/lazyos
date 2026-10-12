//! Image > Resize and Image > Canvas Size, driven through real events by the
//! rectangles the UI probe reports: OK, Cancel, Enter, Esc and bad input.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use xui_core::image::Image;
use xui_core::message::Key;
use xui_paint::Msg;
use xui_paint::model::{Side, Tool};

use common::files::*;

fn disk() -> Rc<Disk> {
    Rc::new(Disk::default())
}

fn open_resize(stage: &Stg<'_>) {
    chord(stage, Key::R, false);
}

fn open_canvas_size(stage: &Stg<'_>) {
    chord(stage, Key::R, true);
}

fn is(image: &Image, x: u32, y: u32, rgb: [u8; 3]) -> bool {
    image.pixel(x, y).is_some_and(|p| {
        p[0].abs_diff(rgb[0]) < 8 && p[1].abs_diff(rgb[1]) < 8 && p[2].abs_diff(rgb[2]) < 8
    })
}

/// The window position of picture pixel `(x, y)` of a `doc`-sized picture.
fn picture_at_size(ctx: &Ctx, doc: (u32, u32), x: i32, y: i32) -> (u32, u32) {
    let canvas = ctx.rect("canvas");
    let mut viewport = xui_paint::viewport::Viewport::new();
    viewport.clamp(doc, (canvas.width(), canvas.height()));
    let (vx, vy) = viewport.to_view(x, y);
    ((canvas.left + vx) as u32, (canvas.top + vy) as u32)
}

#[test]
fn resize_by_percent_scales_the_picture_and_is_one_undo_step() {
    session(&disk(), |stage, ctx| {
        open_resize(stage);
        ctx.probe(stage);
        assert!(ctx.has("resize_percent"), "Ctrl+R opened the dialog");
        type_into(stage, ctx, "resize_percent", "50");
        click(stage, ctx, "resize_ok");
        assert_eq!(ctx.size(), "160 x 120");
        assert_eq!(ctx.status(), "Resized to 160 x 120");
        assert_eq!(ctx.history(), (1, 1), "one step");
        ctx.probe(stage);
        assert!(!ctx.has("resize_ok"), "OK closed the dialog");

        stage.emit(Msg::Undo);
        assert_eq!(ctx.size(), "320 x 240");
        stage.emit(Msg::Redo);
        assert_eq!(ctx.size(), "160 x 120");
    });
}

#[test]
fn resize_by_pixels_keeps_the_aspect_ratio_until_it_is_turned_off() {
    session(&disk(), |stage, ctx| {
        open_resize(stage);
        // The height follows the width (320 x 240 is 4:3).
        type_into(stage, ctx, "resize_width", "640");
        click(stage, ctx, "resize_ok");
        assert_eq!(ctx.size(), "640 x 480");

        open_resize(stage);
        click(stage, ctx, "resize_aspect");
        type_into(stage, ctx, "resize_width", "100");
        type_into(stage, ctx, "resize_height", "30");
        click(stage, ctx, "resize_ok");
        assert_eq!(ctx.size(), "100 x 30", "aspect off: the sides are free");
    });
}

/// A black pixel at picture (10, 10), then resized to 200% with the sampling
/// option at `option` (0 bilinear, 1 nearest); the final frame and the window
/// position of the dot after the resize.
fn dot_after_resize(option: usize) -> (Image, (u32, u32)) {
    let at = Rc::new(Cell::new((0, 0)));
    let out = Rc::clone(&at);
    let image = session(&disk(), move |stage, ctx| {
        drag_picture(stage, ctx, (10, 10), (10, 10));
        open_resize(stage);
        if option == 1 {
            ctx.probe(stage);
            let r = ctx.rect("resize_resample");
            // The second option sits one row (28 dip at 96 dpi) lower.
            click_at(stage, (r.left + 10, r.top + 28 + 14));
        }
        type_into(stage, ctx, "resize_percent", "200");
        click(stage, ctx, "resize_ok");
        ctx.probe(stage);
        out.set(picture_at_size(ctx, (640, 480), 20, 20));
    });
    (image, at.get())
}

#[test]
fn resize_offers_nearest_neighbour_and_bilinear() {
    let (hard, at) = dot_after_resize(1);
    assert!(
        is(&hard, at.0, at.1, [0, 0, 0]),
        "nearest: a solid 2 x 2 dot"
    );
    assert!(is(&hard, at.0 + 1, at.1 + 1, [0, 0, 0]));
    let (soft, at) = dot_after_resize(0);
    assert!(
        !is(&soft, at.0, at.1, [0, 0, 0]),
        "bilinear blends the dot with its white surround"
    );
}

#[test]
fn resize_refuses_bad_input_and_keeps_the_dialog_open() {
    session(&disk(), |stage, ctx| {
        open_resize(stage);
        for bad in ["0", "abc", "-5", "", "100000"] {
            type_into(stage, ctx, "resize_percent", bad);
            click(stage, ctx, "resize_ok");
            assert!(ctx.status().starts_with("Resize failed"), "{bad:?}");
            assert_eq!(ctx.size(), "320 x 240", "{bad:?}");
            assert_eq!(ctx.history(), (0, 0), "{bad:?}");
            ctx.probe(stage);
            assert!(ctx.has("resize_ok"), "{bad:?} left the dialog open");
        }
        // Pixel mode with the aspect ratio free: a side out of range.
        click(stage, ctx, "resize_aspect");
        type_into(stage, ctx, "resize_width", "20000");
        type_into(stage, ctx, "resize_height", "10");
        click(stage, ctx, "resize_ok");
        assert!(ctx.status().contains("Width must be"), "{}", ctx.status());
        assert_eq!(ctx.size(), "320 x 240");
    });
}

#[test]
fn resize_cancel_and_escape_change_nothing_and_enter_accepts() {
    session(&disk(), |stage, ctx| {
        open_resize(stage);
        type_into(stage, ctx, "resize_percent", "10");
        click(stage, ctx, "resize_cancel");
        assert_eq!(ctx.size(), "320 x 240");
        ctx.probe(stage);
        assert!(!ctx.has("resize_ok"));

        open_resize(stage);
        key(stage, Key::ESCAPE);
        ctx.probe(stage);
        assert!(!ctx.has("resize_ok"), "Esc cancels");
        assert_eq!(ctx.size(), "320 x 240");

        // Reopened, the fields start over from the current size.
        open_resize(stage);
        type_into(stage, ctx, "resize_percent", "25");
        key(stage, Key::RETURN);
        assert_eq!(ctx.size(), "80 x 60", "Enter accepts");
    });
}

#[test]
fn a_resize_over_the_memory_budget_is_refused_in_the_status_bar() {
    session(&disk(), |stage, ctx| {
        open_resize(stage);
        click(stage, ctx, "resize_aspect");
        type_into(stage, ctx, "resize_width", "16384");
        type_into(stage, ctx, "resize_height", "16384");
        click(stage, ctx, "resize_ok");
        assert!(
            ctx.status().starts_with("Resize failed"),
            "{}",
            ctx.status()
        );
        assert_eq!(ctx.size(), "320 x 240");
        assert_eq!(ctx.history(), (0, 0));
    });
}

const RED: [u8; 3] = [255, 0, 0];

/// Fills the whole picture red, then grows the canvas to 400 x 300 from the
/// anchor cell `anchor`, `secondary_fill` choosing the new area colour (the
/// secondary is blue). Returns the final frame and the window positions of
/// picture pixels (10, 10) and (390, 290).
fn grown(anchor: usize, secondary_fill: bool) -> (Image, [(u32, u32); 2]) {
    let at = Rc::new(Cell::new([(0, 0); 2]));
    let out = Rc::clone(&at);
    let image = session(&disk(), move |stage, ctx| {
        stage.emit(Msg::Palette {
            color: [255, 0, 0, 255],
            side: Side::Primary,
        });
        stage.emit(Msg::Palette {
            color: [0, 0, 255, 255],
            side: Side::Secondary,
        });
        stage.emit(Msg::Tool(Tool::Fill));
        drag_picture(stage, ctx, (5, 5), (5, 5));
        open_canvas_size(stage);
        type_into(stage, ctx, "canvas_width", "400");
        type_into(stage, ctx, "canvas_height", "300");
        if secondary_fill {
            ctx.probe(stage);
            let r = ctx.rect("canvas_fill");
            // The second option sits one row (28 dip at 96 dpi) lower.
            click_at(stage, (r.left + 10, r.top + 28 + 14));
        }
        click(stage, ctx, &format!("anchor:{anchor}"));
        click(stage, ctx, "canvas_ok");
        assert_eq!(ctx.size(), "400 x 300");
        assert_eq!(ctx.history().0, 2, "the fill and one Canvas Size step");
        ctx.probe(stage);
        out.set([
            picture_at_size(ctx, (400, 300), 10, 10),
            picture_at_size(ctx, (400, 300), 390, 290),
        ]);
    });
    (image, at.get())
}

#[test]
fn canvas_size_anchors_the_picture_at_the_chosen_cell() {
    // Top left: the old red picture keeps the corner, the far corner is new.
    let (image, [near, far]) = grown(0, false);
    assert!(is(&image, near.0, near.1, RED));
    assert!(!is(&image, far.0, far.1, RED), "the new area is not red");
    // Bottom right: the other way round.
    let (image, [near, far]) = grown(8, false);
    assert!(!is(&image, near.0, near.1, RED));
    assert!(is(&image, far.0, far.1, RED));
}

#[test]
fn canvas_size_fills_the_new_area_with_the_secondary_colour_when_asked() {
    let (image, [_, far]) = grown(0, true);
    assert!(is(&image, far.0, far.1, [0, 0, 255]), "the secondary, blue");
    let (image, [_, far]) = grown(0, false);
    assert!(
        !is(&image, far.0, far.1, [0, 0, 255]),
        "transparent shows the checkerboard"
    );
}

#[test]
fn canvas_size_refuses_bad_sides_and_cancels_cleanly() {
    session(&disk(), |stage, ctx| {
        open_canvas_size(stage);
        for bad in ["", "x", "0", "99999"] {
            type_into(stage, ctx, "canvas_width", bad);
            click(stage, ctx, "canvas_ok");
            assert!(
                ctx.status().starts_with("Canvas Size failed"),
                "{bad:?}: {}",
                ctx.status()
            );
            assert_eq!(ctx.size(), "320 x 240", "{bad:?}");
        }
        click(stage, ctx, "canvas_cancel");
        ctx.probe(stage);
        assert!(!ctx.has("canvas_ok"));
        assert_eq!(ctx.history(), (0, 0));

        open_canvas_size(stage);
        key(stage, Key::ESCAPE);
        ctx.probe(stage);
        assert!(!ctx.has("canvas_ok"), "Esc cancels");
    });
}

#[test]
fn the_canvas_size_row_opens_the_dialog_not_a_text_prompt() {
    session(&disk(), |stage, ctx| {
        open_canvas_size(stage);
        ctx.probe(stage);
        assert!(ctx.has("canvas_width") && ctx.has("canvas_height"));
        // Another anchor clicked after one leaves a single cell down; the
        // left-centre anchor keeps the left column of a narrower canvas.
        click(stage, ctx, "anchor:8");
        click(stage, ctx, "anchor:3");
        type_into(stage, ctx, "canvas_width", "100");
        type_into(stage, ctx, "canvas_height", "240");
        click(stage, ctx, "canvas_ok");
        assert_eq!(ctx.size(), "100 x 240");
    });
}
