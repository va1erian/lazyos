//! View tests driven through the offscreen `Stage`: the layout, the tool grid,
//! the Colors panel, canvas drags and the drag lifecycle. No window. The keys,
//! menus and the colour editor are in `keys.rs`.

mod common;

use xui_core::backend::Event;
use xui_core::message::{Key, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Tool;

use common::driver::*;

#[test]
fn the_layout_is_menu_options_grid_canvas_colors_status() {
    session(|_, _, parts| {
        let areas = &parts.areas;
        assert_eq!(areas.menu.top, 0, "the menu bar is first");
        assert_eq!(areas.menu.width(), WIDTH, "and spans the window");
        assert!(
            areas.menu.bottom <= areas.toolbar.top,
            "the options bar sits between"
        );
        assert!(areas.toolbar.bottom <= areas.status.top);
        assert_eq!(areas.toolbar.left, 0, "the tool grid is on the left");
        assert!(areas.toolbar.right <= areas.canvas.left);
        assert!(
            areas.canvas.right <= areas.palette.left,
            "the Colors panel is on the right"
        );
        assert_eq!(areas.palette.right, WIDTH);
        assert_eq!(areas.canvas.top, areas.toolbar.top);
        assert_eq!(areas.status.bottom, HEIGHT);
    });
}

#[test]
fn a_canvas_drag_paints_pixels_and_reports_state() {
    let image = session(|stage, observer, parts| {
        let areas = &parts.areas;
        assert_eq!(status(observer, 0), "--", "no cursor at rest");
        let (x, y) = areas.canvas_at(100, 50);
        drag(stage, (x, y), (x + 100, y + 40), MouseButton::Left);
        let seen = observer.borrow();
        assert!(seen.can_undo, "the stroke is undoable");
        assert_eq!(seen.cursor, Some((200, 90)));
        assert_eq!(seen.status[0], "200, 90");
        assert_eq!(seen.status[2], "Pencil");
    });
    let (x, y) = {
        // The stroke starts at canvas pixel (100, 50).
        let widgets = session_areas();
        widgets.canvas_at(100, 50)
    };
    assert!(
        is(&image, x as u32, y as u32, BLACK),
        "the stroke painted at its start"
    );
}

#[test]
fn tool_grid_clicks_select_tools_and_hovering_names_them() {
    session(|stage, observer, parts| {
        let areas = &parts.areas;
        let line = Tool::ALL
            .iter()
            .position(|tool| *tool == Tool::Line)
            .unwrap() as i32;
        move_to(stage, tool_cell(areas, line));
        assert_eq!(
            status(observer, 3),
            "Line (O)",
            "the hovered tool names itself"
        );
        click(stage, tool_cell(areas, line), MouseButton::Left);
        assert_eq!(observer.borrow().tool, Tool::Line);
        assert_eq!(status(observer, 2), "Line");
        // Leaving the grid takes the hint away.
        move_to(stage, areas.canvas_at(50, 50));
        assert_eq!(status(observer, 3), "");
        // Every cell selects its own tool.
        for (index, tool) in Tool::ALL.into_iter().enumerate() {
            click(stage, tool_cell(areas, index as i32), MouseButton::Left);
            assert_eq!(observer.borrow().tool, tool, "cell {index}");
        }
    });
}

#[test]
fn colour_clicks_take_effect_and_the_size_follows_the_keys() {
    let image = session(|stage, observer, parts| {
        let areas = &parts.areas;
        click(stage, tool_cell(areas, 3), MouseButton::Left); // Line
        stage.emit(Msg::Size(16));
        click(stage, swatch(areas, 3), MouseButton::Left); // red
        {
            let seen = observer.borrow();
            assert_eq!(seen.tool, Tool::Line);
            assert_eq!(seen.size, 16);
            assert_eq!(seen.primary, RED);
        }
        press(stage, Key::from_code(0xDB)); // `[`
        assert_eq!(observer.borrow().size, 12);
        press(stage, Key::from_code(0xDD)); // `]`
        press(stage, Key::from_code(0xDD));
        assert_eq!(observer.borrow().size, 20, "two notches up from 12");
        stage.emit(Msg::Size(16));

        // A shape preview must not be an undo step.
        let (x, y) = areas.canvas_at(120, 80);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 180, y));
        assert!(
            !observer.borrow().can_undo,
            "a preview is not committed to history"
        );
        stage.inject(up(x + 180, y, MouseButton::Left));
        assert!(observer.borrow().can_undo, "releasing commits the shape");
    });
    // A 16px red line from canvas (120, 80) to (300, 80).
    let areas = session_areas();
    let (x, y) = areas.canvas_at(200, 80);
    assert!(is(&image, x as u32, y as u32 + 7, RED), "the shape is red");
    assert!(
        is(&image, x as u32, y as u32 - 7, RED),
        "the shape is 16px thick"
    );
}

#[test]
fn the_palette_buttons_swap_and_reset() {
    session(|stage, observer, parts| {
        let areas = &parts.areas;
        click(stage, swatch(areas, 3), MouseButton::Left); // primary red
        click(stage, swatch(areas, 10), MouseButton::Right); // secondary blue
        assert_eq!(observer.borrow().primary, RED);
        assert_eq!(observer.borrow().secondary, BLUE);
        click(stage, button(areas, 0), MouseButton::Left);
        assert_eq!(observer.borrow().primary, BLUE, "swap");
        assert_eq!(observer.borrow().secondary, RED);
        click(stage, button(areas, 1), MouseButton::Left);
        assert_eq!(observer.borrow().primary, BLACK, "reset");
        assert_eq!(observer.borrow().secondary, WHITE);
        // X swaps and D resets from the keyboard.
        click(stage, swatch(areas, 3), MouseButton::Left);
        press(stage, Key::X);
        assert_eq!(observer.borrow().primary, WHITE);
        assert_eq!(observer.borrow().secondary, RED);
        press(stage, Key::D);
        assert_eq!(observer.borrow().primary, BLACK);
        assert_eq!(observer.borrow().secondary, WHITE);
    });
}

#[test]
fn the_right_button_paints_the_secondary_colour() {
    let image = session(|stage, observer, parts| {
        let areas = &parts.areas;
        click(stage, swatch(areas, 10), MouseButton::Right);
        assert_eq!(observer.borrow().secondary, BLUE);

        let (x, y) = areas.canvas_at(100, 40);
        drag(stage, (x, y), (x + 100, y), MouseButton::Right);
        assert_eq!(observer.borrow().primary, BLACK, "the primary is unchanged");
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(150, 40);
    assert!(
        is(&image, x as u32, y as u32, BLUE),
        "the stroke used the secondary"
    );
}

#[test]
fn fill_then_clear_then_new() {
    let image = session(|stage, observer, parts| {
        let areas = &parts.areas;
        press(stage, Key::F); // Fill
        assert_eq!(observer.borrow().tool, Tool::Fill);
        let (x, y) = areas.canvas_at(100, 100);
        click(stage, (x, y), MouseButton::Left);
        assert!(observer.borrow().can_undo);

        stage.emit(Msg::Clear);
        assert!(observer.borrow().can_undo);

        ctrl(stage, Key::N);
        assert!(
            observer.borrow().can_undo,
            "New asks about the unsaved fill"
        );
        stage.emit(Msg::Unsaved(xui_core::widget::TaskDialogAction::Command(1)));
        assert!(!observer.borrow().can_undo, "New empties the history");
        assert_eq!(observer.borrow().status[1], "320 x 240");
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(100, 100);
    assert!(
        is(&image, x as u32, y as u32, WHITE),
        "the canvas is white again"
    );
}

#[test]
fn a_mouse_up_outside_the_widget_ends_the_drag() {
    let image = session(|stage, observer, parts| {
        let (x, y) = parts.areas.canvas_at(100, 40);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 50, y));
        assert!(observer.borrow().dragging);
        // Release over the status bar, outside the canvas.
        stage.inject(up(x + 50, HEIGHT - 5, MouseButton::Left));
        assert!(!observer.borrow().dragging, "the drag ended");
        assert!(observer.borrow().can_undo);
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(120, 40);
    assert!(is(&image, x as u32, y as u32, BLACK));
}

#[test]
fn a_second_button_mid_drag_does_not_stick() {
    session(|stage, observer, parts| {
        let (x, y) = parts.areas.canvas_at(100, 40);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 40, y));
        // The other button goes down without a release of the first.
        stage.inject(down(x + 80, y, MouseButton::Right));
        assert!(observer.borrow().dragging, "the second drag is live");
        stage.inject(up(x + 80, y, MouseButton::Right));
        assert!(!observer.borrow().dragging, "the tool is not stuck");
    });
}

#[test]
fn a_mouse_leave_mid_drag_does_not_cancel_it() {
    session(|stage, observer, parts| {
        let (x, y) = parts.areas.canvas_at(100, 40);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 50, y));
        stage.inject(Event::MouseLeave);
        assert!(
            observer.borrow().dragging,
            "a leave must not end a captured drag"
        );
        stage.inject(up(x + 50, y, MouseButton::Left));
        assert!(!observer.borrow().dragging);
        assert!(observer.borrow().can_undo);
    });
}

#[test]
fn losing_focus_cancels_an_in_progress_drag() {
    session(|stage, observer, parts| {
        let (x, y) = parts.areas.canvas_at(100, 40);
        drag(stage, (x, y), (x + 50, y), MouseButton::Left);
        // A fresh drag, then focus is lost.
        stage.inject(down(x + 100, y, MouseButton::Left));
        move_to(stage, (x + 140, y));
        assert!(observer.borrow().dragging);
        stage.inject(Event::KillFocus);
        assert!(!observer.borrow().dragging, "focus loss ended the drag");
    });
}

#[test]
fn hovering_the_canvas_updates_the_status_bar() {
    session(|stage, observer, parts| {
        move_to(stage, parts.areas.canvas_at(33, 44));
        assert_eq!(observer.borrow().status[0], "33, 44");
        assert_eq!(observer.borrow().cursor, Some((33, 44)));
        assert_eq!(observer.borrow().status[1], "320 x 240");
    });
}
