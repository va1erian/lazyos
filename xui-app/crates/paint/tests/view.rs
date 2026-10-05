//! View tests driven through the offscreen `Stage`: toolbar, palette, canvas
//! drags, undo/redo and the drag lifecycle. No window.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::Event;
use xui_core::image::Image;
use xui_core::message::{Modifiers, MouseButton};
use xui_paint::model::Tool;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp};

use common::{Areas, Widgets};

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;

/// The strip cell size at the test DPI.
const CELL: i32 = 28;
/// The palette cell size at the test DPI.
const PALETTE_CELL: i32 = 24;

/// The cell centre for strip item `index`.
fn strip(index: i32) -> (i32, i32) {
    (index * CELL + CELL / 2, CELL / 2 + 1)
}

/// The cell centre for palette colour `index`.
fn swatch(areas: &Areas, index: i32) -> (i32, i32) {
    (
        PALETTE_CELL + index * PALETTE_CELL + PALETTE_CELL / 2,
        areas.palette.top + PALETTE_CELL / 2,
    )
}

fn inject(stage: &Stage<'_, xui_paint::Msg>, event: Event) -> bool {
    stage.inject(event)
}

fn down(x: i32, y: i32, button: MouseButton) -> Event {
    Event::MouseDown {
        x,
        y,
        button,
        modifiers: Modifiers::NONE,
    }
}

fn up(x: i32, y: i32, button: MouseButton) -> Event {
    Event::MouseUp {
        x,
        y,
        button,
        modifiers: Modifiers::NONE,
    }
}

fn click(stage: &Stage<'_, xui_paint::Msg>, x: i32, y: i32, button: MouseButton) {
    inject(stage, down(x, y, button));
    inject(stage, up(x, y, button));
}

/// Injects a drag from window `(x0, y0)` to `(x1, y1)` with `button`.
fn drag(stage: &Stage<'_, xui_paint::Msg>, from: (i32, i32), to: (i32, i32), button: MouseButton) {
    inject(stage, down(from.0, from.1, button));
    inject(
        stage,
        Event::MouseMove {
            x: to.0,
            y: to.1,
            modifiers: Modifiers::NONE,
        },
    );
    inject(stage, up(to.0, to.1, button));
}

/// Builds the app with an observer and runs `step` with the placed widget
/// rectangles.
fn session(
    step: impl FnOnce(&Stage<'_, xui_paint::Msg>, &Rc<RefCell<Observer>>, &Areas) + 'static,
) -> Image {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_probe = Rc::clone(&observer);
    let widgets = Widgets::default();
    let built = widgets.clone();
    render_with(
        Snapshot::new(Dip(WIDTH as f32), Dip(HEIGHT as f32)),
        move |ui| {
            built.record(PaintApp::build_observed(
                ui,
                Rc::new(MemoryStorage::new()),
                build_probe,
            ))
        },
        move |stage| step(stage, &observer, &widgets.areas(stage.ui())),
    )
    .expect("render")
}

fn is(bitmap: &Image, x: u32, y: u32, color: [u8; 4]) -> bool {
    bitmap.pixel(x, y).is_some_and(|pixel| {
        pixel[0].abs_diff(color[0]) < 8
            && pixel[1].abs_diff(color[1]) < 8
            && pixel[2].abs_diff(color[2]) < 8
    })
}

#[test]
fn a_canvas_drag_paints_pixels_and_reports_state() {
    let image = session(|stage, observer, areas| {
        let y = areas.canvas.top + 50;
        assert_eq!(observer.borrow().status[0], "--", "no cursor at rest");
        drag(stage, (100, y), (200, y + 40), MouseButton::Left);
        let seen = observer.borrow();
        assert!(seen.can_undo, "the stroke is undoable");
        assert_eq!(seen.cursor, Some((200, 90)));
        assert_eq!(seen.status[0], "200, 90");
        assert_eq!(seen.status[2], "Pencil");
    });
    // The stroke starts at canvas pixel (100, 50).
    assert!(
        is(&image, 100, 78, BLACK),
        "the stroke painted at its start"
    );
}

#[test]
fn toolbar_and_palette_changes_take_effect() {
    let image = session(|stage, observer, areas| {
        let tool = strip(3); // Line
        click(stage, tool.0, tool.1, MouseButton::Left);
        let size = strip(12); // 16 px
        click(stage, size.0, size.1, MouseButton::Left);
        let red = swatch(areas, 3);
        click(stage, red.0, red.1, MouseButton::Left);

        {
            let seen = observer.borrow();
            assert_eq!(seen.tool, Tool::Line);
            assert_eq!(seen.size, 16);
            assert_eq!(seen.primary, RED);
        }

        // A shape preview must not be an undo step.
        let y = areas.canvas.top + 80;
        inject(stage, down(120, y, MouseButton::Left));
        inject(
            stage,
            Event::MouseMove {
                x: 300,
                y,
                modifiers: Modifiers::NONE,
            },
        );
        assert!(
            !observer.borrow().can_undo,
            "a preview is not committed to history"
        );
        inject(stage, up(300, y, MouseButton::Left));
        assert!(observer.borrow().can_undo, "releasing commits the shape");
    });
    // A 16px red line from canvas (120, 80) to (300, 80).
    assert!(is(&image, 200, 108, RED), "the shape is red");
    assert!(is(&image, 200, 100, RED), "the shape is 16px thick");
}

#[test]
fn undo_and_redo_buttons_work() {
    let image = session(|stage, observer, areas| {
        let y = areas.canvas.top + 40;
        drag(stage, (100, y), (200, y), MouseButton::Left);
        assert!(observer.borrow().can_undo);

        let undo = strip(13);
        click(stage, undo.0, undo.1, MouseButton::Left);
        assert!(!observer.borrow().can_undo, "undo consumed the step");
        assert!(observer.borrow().can_redo);

        let redo = strip(14);
        click(stage, redo.0, redo.1, MouseButton::Left);
        assert!(observer.borrow().can_undo, "redo restored the step");
        assert!(!observer.borrow().can_redo);
    });
    assert!(is(&image, 150, 68, BLACK), "the redone stroke is back");
}

#[test]
fn the_right_button_paints_the_secondary_colour() {
    let image = session(|stage, observer, areas| {
        let blue = swatch(areas, 10);
        click(stage, blue.0, blue.1, MouseButton::Right);
        assert_eq!(observer.borrow().secondary, BLUE);

        let y = areas.canvas.top + 40;
        drag(stage, (100, y), (200, y), MouseButton::Right);
        assert_eq!(observer.borrow().primary, BLACK, "the primary is unchanged");
    });
    assert!(is(&image, 150, 68, BLUE), "the stroke used the secondary");
}

#[test]
fn fill_then_clear_then_new() {
    let image = session(|stage, observer, areas| {
        let fill = strip(6);
        click(stage, fill.0, fill.1, MouseButton::Left);
        // Inside the 320x240 bitmap, not merely inside the 800-wide viewport.
        click(stage, 200, areas.canvas.top + 100, MouseButton::Left);
        assert!(observer.borrow().can_undo);

        let clear = strip(15);
        click(stage, clear.0, clear.1, MouseButton::Left);
        assert!(observer.borrow().can_undo);

        let new = strip(16);
        click(stage, new.0, new.1, MouseButton::Left);
        assert!(!observer.borrow().can_undo, "New empties the history");
        assert_eq!(observer.borrow().status[1], "320 x 240");
    });
    assert!(is(&image, 200, 128, WHITE), "the canvas is white again");
}

#[test]
fn a_mouse_up_outside_the_widget_ends_the_drag() {
    let image = session(|stage, observer, areas| {
        let y = areas.canvas.top + 40;
        inject(stage, down(100, y, MouseButton::Left));
        inject(
            stage,
            Event::MouseMove {
                x: 150,
                y,
                modifiers: Modifiers::NONE,
            },
        );
        assert!(observer.borrow().dragging);
        // Release over the status bar, outside the canvas.
        inject(stage, up(150, HEIGHT - 5, MouseButton::Left));
        assert!(!observer.borrow().dragging, "the drag ended");
        assert!(observer.borrow().can_undo);
    });
    assert!(is(&image, 120, 68, BLACK));
}

#[test]
fn a_second_button_mid_drag_does_not_stick() {
    session(|stage, observer, areas| {
        let y = areas.canvas.top + 40;
        inject(stage, down(100, y, MouseButton::Left));
        inject(
            stage,
            Event::MouseMove {
                x: 140,
                y,
                modifiers: Modifiers::NONE,
            },
        );
        // The other button goes down without a release of the first.
        inject(stage, down(180, y, MouseButton::Right));
        assert!(observer.borrow().dragging, "the second drag is live");
        inject(stage, up(180, y, MouseButton::Right));
        assert!(!observer.borrow().dragging, "the tool is not stuck");
    });
}

#[test]
fn a_mouse_leave_mid_drag_does_not_cancel_it() {
    session(|stage, observer, areas| {
        let y = areas.canvas.top + 40;
        inject(stage, down(100, y, MouseButton::Left));
        inject(
            stage,
            Event::MouseMove {
                x: 150,
                y,
                modifiers: Modifiers::NONE,
            },
        );
        inject(stage, Event::MouseLeave);
        assert!(
            observer.borrow().dragging,
            "a leave must not end a captured drag"
        );
        inject(stage, up(150, y, MouseButton::Left));
        assert!(!observer.borrow().dragging);
        assert!(observer.borrow().can_undo);
    });
}

#[test]
fn losing_focus_cancels_an_in_progress_drag() {
    session(|stage, observer, areas| {
        let y = areas.canvas.top + 40;
        drag(stage, (100, y), (150, y), MouseButton::Left);
        // A fresh drag, then focus is lost.
        inject(stage, down(200, y, MouseButton::Left));
        inject(
            stage,
            Event::MouseMove {
                x: 240,
                y,
                modifiers: Modifiers::NONE,
            },
        );
        assert!(observer.borrow().dragging);
        inject(stage, Event::KillFocus);
        assert!(!observer.borrow().dragging, "focus loss ended the drag");
    });
}

#[test]
fn hovering_the_canvas_updates_the_status_bar() {
    session(|stage, observer, areas| {
        inject(
            stage,
            Event::MouseMove {
                x: 33,
                y: areas.canvas.top + 44,
                modifiers: Modifiers::NONE,
            },
        );
        assert_eq!(observer.borrow().status[0], "33, 44");
        assert_eq!(observer.borrow().cursor, Some((33, 44)));
        assert_eq!(observer.borrow().status[1], "320 x 240");
    });
}
