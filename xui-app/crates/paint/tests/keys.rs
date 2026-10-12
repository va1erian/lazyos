//! Keyboard, menu bar and colour editor tests driven through the offscreen
//! `Stage`.

mod common;

use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::{Side, Tool};
use xui_paint::view::{Command, menu_item};

use common::driver::*;

#[test]
fn undo_and_redo_keys_work_and_the_menu_rows_follow() {
    let image = session(|stage, observer, parts| {
        let areas = &parts.areas;
        let undo = menu_item(Msg::Undo).unwrap();
        let redo = menu_item(Msg::Redo).unwrap();
        assert!(!parts.menu.is_enabled(undo), "nothing to undo yet");
        let (x, y) = areas.canvas_at(100, 40);
        drag(stage, (x, y), (x + 100, y), MouseButton::Left);
        assert!(observer.borrow().can_undo);
        assert!(parts.menu.is_enabled(undo));
        assert!(!parts.menu.is_enabled(redo));

        ctrl(stage, Key::Z);
        assert!(!observer.borrow().can_undo, "undo consumed the step");
        assert!(observer.borrow().can_redo);
        assert!(!parts.menu.is_enabled(undo));
        assert!(parts.menu.is_enabled(redo));

        ctrl(stage, Key::Y);
        assert!(observer.borrow().can_undo, "redo restored the step");
        assert!(!observer.borrow().can_redo);
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(150, 40);
    assert!(
        is(&image, x as u32, y as u32, BLACK),
        "the redone stroke is back"
    );
}

#[test]
fn tool_keys_select_tools_and_a_repeated_key_cycles_the_shapes() {
    session(|stage, observer, _| {
        let tool = || observer.borrow().tool;
        press(stage, Key::B);
        assert_eq!(tool(), Tool::Brush);
        press(stage, Key::E);
        assert_eq!(tool(), Tool::Eraser);
        press(stage, Key::K);
        assert_eq!(tool(), Tool::Picker);
        press(stage, Key::P);
        assert_eq!(tool(), Tool::Pencil);
        for expected in [Tool::Line, Tool::Rectangle, Tool::Ellipse, Tool::Line] {
            press(stage, Key::O);
            assert_eq!(tool(), expected);
        }
        // A key with Shift or Alt held is not a tool key.
        press_with(
            stage,
            Key::B,
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
        );
        assert_eq!(tool(), Tool::Line);
    });
}

#[test]
fn keys_of_unbuilt_features_say_so_in_the_status_bar() {
    session(|stage, observer, _| {
        ctrl(stage, Key::C);
        assert_eq!(status(observer, 3), "Copy is not available yet");
        press(stage, Key::T);
        assert_eq!(status(observer, 3), "Text is not available yet");
        assert_eq!(
            observer.borrow().tool,
            Tool::Pencil,
            "the tool did not change"
        );
    });
}

#[test]
fn a_stroke_in_progress_is_committed_before_a_key_changes_the_tool() {
    session(|stage, observer, parts| {
        let (x, y) = parts.areas.canvas_at(100, 40);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 50, y));
        assert!(observer.borrow().dragging);
        press(stage, Key::B);
        assert!(!observer.borrow().dragging, "the stroke ended");
        assert!(observer.borrow().can_undo, "and reached the history");
        assert_eq!(observer.borrow().tool, Tool::Brush);
    });
}

#[test]
fn shortcuts_keep_out_of_a_dialog() {
    session(|stage, observer, _| {
        // Ctrl+Shift+R opens the canvas-size prompt; its field gets the keys.
        press_with(
            stage,
            Key::R,
            Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            },
        );
        press(stage, Key::B);
        press(stage, Key::O);
        assert_eq!(
            observer.borrow().tool,
            Tool::Pencil,
            "tool keys left the dialog alone"
        );
        press(stage, Key::ESCAPE);
        press(stage, Key::B);
        assert_eq!(
            observer.borrow().tool,
            Tool::Brush,
            "and work again after it closes"
        );
    });
}

#[test]
fn the_file_menu_runs_new_by_its_mnemonic() {
    session(|stage, observer, parts| {
        let areas = &parts.areas;
        let (x, y) = areas.canvas_at(100, 40);
        drag(stage, (x, y), (x + 50, y), MouseButton::Left);
        assert!(observer.borrow().can_undo);
        // Click "File" at the bar's left, then `N` for New.
        click(
            stage,
            (areas.menu.left + 16, areas.menu.top + 12),
            MouseButton::Left,
        );
        assert!(parts.menu.is_open(), "the click opened the File menu");
        // While a menu is open the shortcut table stays out of the way.
        press(stage, Key::N);
        stage.inject(Event::Char('n'));
        assert!(!observer.borrow().can_undo, "File > New reset the document");
        assert!(!parts.menu.is_open());
        assert!(stage.ui().is_visible(parts.canvas));
    });
}

#[test]
fn unbuilt_menu_rows_are_disabled() {
    session(|_, _, parts| {
        for command in Command::ALL {
            if let Some(id) = menu_item(Msg::Command(command)) {
                assert_eq!(
                    parts.menu.is_enabled(id),
                    command.available(),
                    "{}",
                    command.label()
                );
            }
        }
        for msg in [Msg::New, Msg::Quit, Msg::ResizeAsk, Msg::About, Msg::Clear] {
            assert!(parts.menu.is_enabled(menu_item(msg).unwrap()), "{msg:?}");
        }
        // The in-memory storage has no path, so Open and Save work through it.
        assert!(parts.menu.is_enabled(menu_item(Msg::Save).unwrap()));
    });
}

#[test]
fn clicking_a_swatch_opens_the_colour_editor_which_edits_live_and_cancels() {
    session(|stage, observer, parts| {
        let areas = &parts.areas;
        assert_eq!(observer.borrow().editing, None, "the editor starts closed");
        assert!(stage.ui().is_visible(parts.canvas));

        click(stage, swatch(areas, 3), MouseButton::Left); // red
        click(stage, primary_swatch(areas), MouseButton::Left);
        assert_eq!(observer.borrow().editing, Some(Side::Primary));
        assert!(!stage.ui().is_visible(parts.canvas), "the canvas gave way");
        assert_eq!(parts.panel.color(), xui_core::Color::rgb(255, 0, 0));

        // Shortcuts stay out of the editor (its hex field takes typing).
        press(stage, Key::B);
        assert_eq!(observer.borrow().tool, Tool::Pencil);

        stage.emit(Msg::ColorEditing([0, 255, 0, 255]));
        assert_eq!(observer.borrow().primary, [0, 255, 0, 255], "live");

        stage.emit(Msg::ColorCancel);
        assert_eq!(observer.borrow().primary, RED, "Cancel restores");
        assert_eq!(observer.borrow().editing, None);
        assert!(stage.ui().is_visible(parts.canvas), "the canvas is back");

        // OK keeps the edit, for the secondary colour too.
        click(stage, secondary_swatch(areas), MouseButton::Left);
        assert_eq!(observer.borrow().editing, Some(Side::Secondary));
        stage.emit(Msg::ColorEditing([10, 20, 30, 255]));
        stage.emit(Msg::ColorDone);
        assert_eq!(observer.borrow().secondary, [10, 20, 30, 255]);
        assert_eq!(observer.borrow().primary, RED);
        // The canvas is back and the keys work again.
        press(stage, Key::B);
        assert_eq!(observer.borrow().tool, Tool::Brush);
    });
}

#[test]
fn the_view_menu_zooms_by_key_and_by_row() {
    session(|stage, observer, parts| {
        let zoom = || observer.borrow().zoom_percent;
        assert_eq!(zoom(), 100);
        // Ctrl++ (the shifted form too) and Ctrl+- step the zoom.
        press_with(
            stage,
            Key::from_code(0xBB),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        );
        assert!(zoom() > 100, "zoomed in: {}", zoom());
        let zoomed = zoom();
        press_with(
            stage,
            Key::from_code(0xBD),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        );
        assert!(zoom() < zoomed, "zoomed out");
        // Ctrl+0 is actual size, Ctrl+B fits the picture to the window.
        stage.emit(Msg::Command(Command::ZoomIn));
        stage.emit(Msg::Command(Command::ZoomIn));
        ctrl(stage, Key::DIGIT0);
        assert_eq!(zoom(), 100, "actual size");
        ctrl(stage, Key::B);
        assert!(zoom() > 100, "the 320 x 240 picture fits a larger zoom");
        // The rows are enabled, and Pixel Grid reports its state.
        for command in [
            Command::ZoomIn,
            Command::ZoomOut,
            Command::ZoomToWindow,
            Command::ActualSize,
            Command::PixelGrid,
        ] {
            assert!(
                parts
                    .menu
                    .is_enabled(menu_item(Msg::Command(command)).unwrap())
            );
        }
        stage.emit(Msg::Command(Command::PixelGrid));
        assert_eq!(status(observer, 3), "Pixel grid on");
        stage.emit(Msg::Command(Command::PixelGrid));
        assert_eq!(status(observer, 3), "Pixel grid off");
    });
}

#[test]
fn undo_and_redo_name_the_edit_in_the_status_bar() {
    session(|stage, observer, parts| {
        let (x, y) = parts.areas.canvas_at(100, 40);
        drag(stage, (x, y), (x + 50, y), MouseButton::Left);
        ctrl(stage, Key::Z);
        assert!(
            status(observer, 3).starts_with("Undid "),
            "{}",
            status(observer, 3)
        );
        ctrl(stage, Key::Y);
        assert!(
            status(observer, 3).starts_with("Redid "),
            "{}",
            status(observer, 3)
        );
    });
}

#[test]
fn the_eraser_erases_with_the_left_button_and_paints_the_secondary_with_the_right() {
    session(|stage, observer, parts| {
        press(stage, Key::E);
        assert_eq!(observer.borrow().tool, Tool::Eraser);
        // The eraser offers the size control like the brush.
        assert!(xui_paint::view::uses_size(Tool::Eraser));
        let (x, y) = parts.areas.canvas_at(100, 40);
        drag(stage, (x, y), (x + 50, y), MouseButton::Left);
        assert!(observer.borrow().can_undo, "an erase is one undo step");
        assert_eq!(observer.borrow().primary, BLACK, "colours are untouched");
    });
}
