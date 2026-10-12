//! Regressions from the Paint v1 review, driven through the offscreen `Stage`:
//! keys that belong to a field, the unsaved-changes bookkeeping, Esc during a
//! Move Selection drag, the frames' Enter and Esc, the window's close button,
//! the Layers panel scroll and the adjustment preview on the active layer.
//!
//! The offscreen backend never moves the focus on a click or sends focus
//! events, so "a field has the keyboard" is staged by sending the canvas the
//! `KillFocus` a real backend sends it.

mod common;

use std::rc::Rc;

use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_core::widget::TaskDialogAction;
use xui_paint::Msg;
use xui_paint::model::{Side, Tool};
use xui_paint::view::Command;

use common::driver as d;
use common::files::*;

/// The canvas loses the keyboard to the options bar.
fn field_takes_keyboard(stage: &Stg<'_>) {
    stage.inject(Event::KillFocus);
}

/// And gets it back.
fn canvas_takes_keyboard(stage: &Stg<'_>) {
    stage.inject(Event::SetFocus);
}

fn disk() -> Rc<Disk> {
    Rc::new(Disk::default())
}

#[test]
fn plain_keys_are_left_to_a_field_that_has_the_keyboard() {
    session(&disk(), |stage, ctx| {
        stage.emit(Msg::Tool(Tool::Brush));
        chord(stage, Key::A, false);
        let before = ctx.history();
        field_takes_keyboard(stage);
        // Backspace (Fill Selection), Delete (Erase), a tool letter, brackets.
        for k in [Key::BACK, Key::DELETE, Key::E, Key::P] {
            key(stage, k);
        }
        assert_eq!(ctx.history(), before, "no command ran");
        assert_eq!(ctx.observer.borrow().tool, Tool::Brush, "no tool switch");
        // A Ctrl chord still works with the field focused.
        chord(stage, Key::D, false);
        assert!(ctx.observer.borrow().selection.is_none());
        // With the keyboard back on the canvas, the same keys act.
        canvas_takes_keyboard(stage);
        key(stage, Key::E);
        assert_eq!(ctx.observer.borrow().tool, Tool::Eraser);
    });
}

#[test]
fn arrows_do_not_nudge_a_float_while_a_field_has_the_keyboard() {
    session(&disk(), |stage, ctx| {
        stage.emit(Msg::Tool(Tool::MovePixels));
        chord(stage, Key::A, false);
        stage.emit(Msg::Nudge(0, 0));
        field_takes_keyboard(stage);
        let at = ctx.observer.borrow().selection;
        key(stage, Key::RIGHT);
        assert_eq!(
            ctx.observer.borrow().selection,
            at,
            "the arrow stayed in the field"
        );
        canvas_takes_keyboard(stage);
        key(stage, Key::RIGHT);
        assert_ne!(
            ctx.observer.borrow().selection,
            at,
            "and nudges on the canvas"
        );
    });
}

#[test]
fn paste_into_new_image_asks_before_replacing_unsaved_changes() {
    session(&disk(), |stage, ctx| {
        chord(stage, Key::A, false);
        chord(stage, Key::C, false);
        stage.emit(Msg::Command(Command::EraseSelection));
        let edited = ctx.history();
        stage.emit(Msg::Command(Command::PasteNewImage));
        assert_eq!(ctx.history(), edited, "waiting for the answer");
        stage.emit(Msg::Unsaved(TaskDialogAction::Cancel));
        assert_eq!(ctx.history(), edited, "Cancel kept the picture");
        stage.emit(Msg::Command(Command::PasteNewImage));
        stage.emit(Msg::Unsaved(TaskDialogAction::Command(1)));
        assert_eq!(ctx.history(), (0, 0), "Don't Save replaced it");
    });
}

#[test]
fn esc_during_a_move_selection_drag_takes_the_drag_back() {
    session(&disk(), |stage, ctx| {
        stage.emit(Msg::Tool(Tool::RectSelect));
        drag_picture(stage, ctx, (50, 50), (100, 100));
        let selected = ctx.observer.borrow().selection;
        let steps = ctx.history();
        stage.emit(Msg::Tool(Tool::MoveSelection));
        ctx.probe(stage);
        let (x, y) = picture_at(ctx, 70, 70);
        let (x, y) = (x as i32, y as i32);
        stage.inject(Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        stage.inject(Event::MouseMove {
            x: x + 40,
            y,
            modifiers: Modifiers::NONE,
        });
        assert_ne!(ctx.observer.borrow().selection, selected, "it moved");
        key(stage, Key::ESCAPE);
        assert_eq!(ctx.observer.borrow().selection, selected, "Esc put it back");
        assert_eq!(ctx.history().0, steps.0, "and left no step");
        stage.inject(Event::MouseUp {
            x: x + 40,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        assert_eq!(
            ctx.observer.borrow().selection,
            selected,
            "the release changed nothing"
        );
    });
}

/// Whether the path-less app (Save writes through its storage) has unsaved
/// changes.
fn unsaved(observer: &std::rc::Rc<std::cell::RefCell<xui_paint::Observer>>) -> bool {
    observer.borrow().dirty
}

#[test]
fn selections_layer_clicks_and_cancelled_properties_are_not_unsaved_edits() {
    d::session(|stage, observer, _| {
        stage.emit(Msg::Command(Command::AddLayer));
        stage.emit(Msg::Save);
        assert!(!unsaved(observer), "saved");
        // Selecting layers and opening and cancelling Layer Properties.
        stage.emit(Msg::LayerSelect(0));
        stage.emit(Msg::LayerSelect(1));
        assert!(!unsaved(observer), "a layer click is no edit");
        stage.emit(Msg::LayerProps(0));
        stage.emit(Msg::PropsChanged);
        stage.emit(Msg::PropsCancel);
        assert!(!unsaved(observer), "a cancelled Properties is none either");
        // A selection is a history step (as in Paint.NET), so it counts, but
        // restoring the saved selection is clean again.
        d::ctrl(stage, Key::A);
        assert!(unsaved(observer), "Select All changes the picture's state");
        d::ctrl(stage, Key::D);
        assert!(!unsaved(observer), "Deselect is back where the file is");
        // Undo to the saved state is clean; a real edit is not.
        d::ctrl(stage, Key::A);
        d::ctrl(stage, Key::Z);
        assert!(!unsaved(observer));
        stage.emit(Msg::Command(Command::InvertColors));
        assert!(unsaved(observer));
        d::ctrl(stage, Key::Z);
        assert!(!unsaved(observer), "undo returns to the saved entry");
        d::ctrl(stage, Key::Y);
        assert!(unsaved(observer));
    });
}

#[test]
fn a_new_edit_after_undo_is_not_the_saved_state() {
    d::session(|stage, observer, _| {
        stage.emit(Msg::Command(Command::InvertColors));
        stage.emit(Msg::Save);
        assert!(!unsaved(observer));
        d::ctrl(stage, Key::Z);
        assert!(unsaved(observer));
        // Another edit with the same history length as the saved one.
        stage.emit(Msg::Command(Command::Sepia));
        assert!(unsaved(observer), "a different entry at the saved depth");
    });
}

#[test]
fn frames_answer_enter_and_esc_and_the_close_button_cancels_them() {
    session(&disk(), |stage, ctx| {
        // Resize: Esc cancels, Enter is OK.
        chord(stage, Key::R, false);
        ctx.probe(stage);
        assert!(ctx.has("resize_ok"));
        key(stage, Key::ESCAPE);
        ctx.probe(stage);
        assert!(!ctx.has("resize_ok"), "Esc closed it");
        chord(stage, Key::R, false);
        key(stage, Key::RETURN);
        ctx.probe(stage);
        assert!(!ctx.has("resize_ok"), "Enter closed it");

        // The colour editor: Esc restores the colour, Enter keeps it.
        stage.emit(Msg::EditColor(Side::Primary));
        stage.emit(Msg::ColorEditing([1, 2, 3, 255]));
        key(stage, Key::ESCAPE);
        assert_eq!(ctx.observer.borrow().primary, [0, 0, 0, 255]);
        assert!(ctx.observer.borrow().editing.is_none());
        stage.emit(Msg::EditColor(Side::Primary));
        stage.emit(Msg::ColorEditing([1, 2, 3, 255]));
        key(stage, Key::RETURN);
        assert_eq!(ctx.observer.borrow().primary, [1, 2, 3, 255]);
        assert!(ctx.observer.borrow().editing.is_none());

        // Layer Properties: Esc cancels, Enter keeps.
        stage.emit(Msg::LayerProps(0));
        assert!(ctx.observer.borrow().props_open);
        key(stage, Key::ESCAPE);
        assert!(!ctx.observer.borrow().props_open);
        stage.emit(Msg::LayerProps(0));
        key(stage, Key::RETURN);
        assert!(!ctx.observer.borrow().props_open);

        // The close button with a frame open and unsaved changes: the frame
        // is cancelled and the Save question follows.
        stage.emit(Msg::Command(Command::InvertColors));
        chord(stage, Key::R, false);
        stage.emit(Msg::Quit);
        ctx.probe(stage);
        assert!(!ctx.has("resize_ok"), "the frame was cancelled");
        assert!(ctx.dirty(), "and the picture is still here, asking");
    });
}

#[test]
fn the_layers_panel_keeps_a_wheel_scroll_across_messages() {
    session(&disk(), |stage, ctx| {
        for _ in 0..9 {
            stage.emit(Msg::Command(Command::AddLayer));
        }
        ctx.probe(stage);
        let panel = ctx.rect("layers");
        let (px, py) = (
            (panel.left + panel.right) / 2,
            (panel.top + panel.bottom) / 2,
        );
        let top_of = |ctx: &Ctx| ctx.rect("layer:9").top;
        let revealed = top_of(ctx);
        stage.inject(Event::MouseWheel {
            delta: -3,
            horizontal: false,
            x: px,
            y: py,
            modifiers: Modifiers::NONE,
        });
        ctx.probe(stage);
        let scrolled = top_of(ctx);
        assert_ne!(scrolled, revealed, "the wheel scrolled the list");
        stage.emit(Msg::HintTool(None));
        stage.emit(Msg::SizeStep(1));
        ctx.probe(stage);
        assert_eq!(top_of(ctx), scrolled, "other messages left it there");
    });
}

#[test]
fn the_adjustment_preview_changes_only_the_active_layer() {
    let canvas_centre = Rc::new(std::cell::Cell::new((0u32, 0u32)));
    let out = Rc::clone(&canvas_centre);
    let image = session(&disk(), move |stage, ctx| {
        stage.emit(Msg::Palette {
            color: [220, 40, 40, 255],
            side: Side::Primary,
        });
        stage.emit(Msg::Tool(Tool::Fill));
        drag_picture(stage, ctx, (5, 5), (5, 5));
        // A new, empty layer is the active one: OK would change nothing seen.
        stage.emit(Msg::Command(Command::AddLayer));
        stage.emit(Msg::Command(Command::HueSaturation));
        click_along(stage, ctx, "adjust_slider:0", 0.75);
        out.set(picture_at(ctx, 100, 100));
    });
    let [r, g, b] = rgb(&image, canvas_centre.get());
    assert!(
        r.abs_diff(220) < 12 && g.abs_diff(40) < 12 && b.abs_diff(40) < 12,
        "the red layer below is untouched in the preview: {r},{g},{b}"
    );
}

#[test]
fn dpi_scaled_lengths_follow_the_window() {
    use xui_paint::viewport::{scaled, scaled_px};
    assert_eq!(scaled_px(8, 96), 8);
    assert_eq!(scaled_px(8, 192), 16);
    assert_eq!(scaled_px(1, 48), 1, "never below one pixel");
    assert!((scaled(10.0, 144) - 15.0).abs() < 1e-9);
}
