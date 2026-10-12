//! The Adjustments menu: the instant ones, their clip to the selection, and
//! the Brightness/Contrast and Hue/Saturation dialogs with their live preview.
//! The preview must leave the document alone until OK.

mod common;

use std::rc::Rc;

use xui_core::message::Key;
use xui_paint::Msg;
use xui_paint::model::{Side, Tool};
use xui_paint::view::Command;

use common::files::*;

fn disk() -> Rc<Disk> {
    Rc::new(Disk::default())
}

const INSIDE: (i32, i32) = (100, 100);
const OUTSIDE: (i32, i32) = (250, 200);
static PICKS: [(i32, i32); 2] = [INSIDE, OUTSIDE];

fn fill_with(stage: &Stg<'_>, ctx: &Ctx, color: [u8; 4]) {
    stage.emit(Msg::Palette {
        color,
        side: Side::Primary,
    });
    stage.emit(Msg::Tool(Tool::Fill));
    drag_picture(stage, ctx, (5, 5), (5, 5));
    stage.emit(Msg::Tool(Tool::Pencil));
}

fn red(stage: &Stg<'_>, ctx: &Ctx) {
    fill_with(stage, ctx, [255, 0, 0, 255]);
}

fn select(stage: &Stg<'_>, ctx: &Ctx, from: (i32, i32), to: (i32, i32)) {
    stage.emit(Msg::Tool(Tool::RectSelect));
    drag_picture(stage, ctx, from, to);
}

fn near(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= tolerance)
}

#[test]
fn the_instant_adjustments_apply_through_their_keys() {
    // Invert: white becomes black.
    let (image, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        chord(stage, Key::I, true);
        assert_eq!(ctx.history(), (1, 1), "one step");
        assert_eq!(ctx.status(), "Invert Colors");
    });
    assert_eq!(rgb(&image, at[0]), [0, 0, 0]);

    // Black and White: red becomes its luma grey (Rec. 601: 76).
    let (image, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        red(stage, ctx);
        chord(stage, Key::G, true);
    });
    let grey = rgb(&image, at[0]);
    assert!(grey[0] == grey[1] && grey[1] == grey[2], "{grey:?}");
    assert!(grey[0].abs_diff(76) <= 2, "{grey:?}");

    // Sepia: white turns warm, not grey.
    let (image, at) = session_pixels(&disk(), &PICKS, |stage, _| chord(stage, Key::E, true));
    let warm = rgb(&image, at[0]);
    assert!(warm[0] >= warm[1] && warm[1] > warm[2], "{warm:?}");
}

#[test]
fn adjustments_are_clipped_to_the_selection() {
    let (image, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        select(stage, ctx, (50, 50), (150, 150));
        chord(stage, Key::I, true);
    });
    assert_eq!(rgb(&image, at[0]), [0, 0, 0], "inside is inverted");
    assert_eq!(rgb(&image, at[1]), [255, 255, 255], "outside is not");
}

#[test]
fn instant_adjustments_undo_and_redo_as_single_steps() {
    session(&disk(), |stage, ctx| {
        chord(stage, Key::I, true);
        chord(stage, Key::I, true);
        assert_eq!(ctx.history(), (2, 2));
        stage.emit(Msg::Undo);
        assert_eq!(ctx.history(), (1, 2));
        stage.emit(Msg::Undo);
        assert_eq!(ctx.history(), (0, 2));
    });
}

/// Opens the strip for `command`, with the first slider clicked at
/// `fraction` of its length.
fn open_and_drag(stage: &Stg<'_>, ctx: &Ctx, command: Command, fraction: f32) {
    stage.emit(Msg::Command(command));
    ctx.probe(stage);
    assert!(ctx.has("adjust_slider:0"), "the strip opened");
    click_along(stage, ctx, "adjust_slider:0", fraction);
}

#[test]
fn brightness_contrast_previews_on_screen_and_leaves_the_document_alone() {
    let (image, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
        // Nothing reached the document: no step, no unsaved change.
        assert_eq!(ctx.history(), (0, 0));
        assert!(!ctx.dirty());
    });
    let shown = rgb(&image, at[0]);
    assert!(
        shown[0] < 245,
        "the preview darkened the picture: {shown:?}"
    );
    assert_eq!(shown[0], shown[1]);
}

#[test]
fn cancel_drops_the_preview_and_ok_applies_what_the_preview_showed() {
    let (preview, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
    });
    let (cancelled, at_c) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
        click(stage, ctx, "adjust_cancel");
        assert_eq!(ctx.history(), (0, 0));
        assert!(!ctx.dirty());
    });
    assert_eq!(rgb(&cancelled, at_c[0]), [255, 255, 255], "restored");

    let (applied, at_a) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
        click(stage, ctx, "adjust_ok");
        assert_eq!(ctx.history(), (1, 1), "OK is one history entry");
        assert!(ctx.dirty());
        assert_eq!(ctx.status(), "Brightness/Contrast");
        ctx.probe(stage);
        assert!(!ctx.has("adjust_ok"), "OK closed the strip");
    });
    // The preview and the applied pixels are the same colour.
    assert!(
        near(rgb(&preview, at[0]), rgb(&applied, at_a[0]), 1),
        "{:?} vs {:?}",
        rgb(&preview, at[0]),
        rgb(&applied, at_a[0])
    );
    assert_eq!(rgb(&applied, at_a[1]), rgb(&applied, at_a[0]));
}

#[test]
fn undoing_an_applied_adjustment_restores_the_layer() {
    let (image, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
        click(stage, ctx, "adjust_ok");
        stage.emit(Msg::Undo);
        assert_eq!(ctx.history(), (0, 1));
    });
    assert_eq!(rgb(&image, at[0]), [255, 255, 255]);
}

#[test]
fn hue_saturation_previews_through_the_selection_and_applies_on_ok() {
    let (preview, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        red(stage, ctx);
        select(stage, ctx, (50, 50), (150, 150));
        let before = ctx.history();
        open_and_drag(stage, ctx, Command::HueSaturation, 0.75);
        ctx.probe(stage);
        assert!(ctx.has("adjust_slider:2"), "three sliders");
        assert_eq!(ctx.history(), before, "the preview is not a step");
    });
    let inside = rgb(&preview, at[0]);
    assert!(inside[0] < 240 || inside[1] > 20, "turned: {inside:?}");
    assert_eq!(rgb(&preview, at[1]), [255, 0, 0], "outside the selection");

    let (applied, at) = session_pixels(&disk(), &PICKS, |stage, ctx| {
        red(stage, ctx);
        select(stage, ctx, (50, 50), (150, 150));
        let before = ctx.history().0;
        open_and_drag(stage, ctx, Command::HueSaturation, 0.75);
        click(stage, ctx, "adjust_ok");
        assert_eq!(ctx.history().0, before + 1, "OK is one step");
    });
    assert!(
        near(rgb(&applied, at[0]), inside, 2),
        "OK matches the preview"
    );
    assert_eq!(rgb(&applied, at[1]), [255, 0, 0]);
}

#[test]
fn enter_applies_and_escape_cancels_a_strip() {
    session(&disk(), |stage, ctx| {
        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
        key(stage, Key::ESCAPE);
        assert_eq!(ctx.history(), (0, 0));
        ctx.probe(stage);
        assert!(!ctx.has("adjust_ok"));

        open_and_drag(stage, ctx, Command::BrightnessContrast, 0.2);
        key(stage, Key::RETURN);
        assert_eq!(ctx.history(), (1, 1));
    });
}

#[test]
fn a_neutral_strip_applies_nothing() {
    session(&disk(), |stage, ctx| {
        stage.emit(Msg::Command(Command::HueSaturation));
        click(stage, ctx, "adjust_ok");
        assert_eq!(ctx.history(), (0, 0), "no pixel changed, no step");
        assert!(!ctx.dirty());
    });
}

#[test]
fn shortcuts_and_the_canvas_stay_out_while_a_strip_is_open() {
    session(&disk(), |stage, ctx| {
        stage.emit(Msg::Command(Command::BrightnessContrast));
        chord(stage, Key::I, true);
        assert_eq!(ctx.history(), (0, 0), "Invert did not run under the strip");
        drag_picture(stage, ctx, (10, 10), (60, 60));
        assert_eq!(ctx.history(), (0, 0), "the canvas ignored the pointer");
    });
}
