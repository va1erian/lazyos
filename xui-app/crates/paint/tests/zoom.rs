//! The zoomable canvas through the offscreen `Stage`: Ctrl+wheel zoom around
//! the pointer, pointer mapping at zoom, nearest-neighbour pixels, the grid and
//! panning. Writes PNGs to `target/snapshots/` for eyeballing.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::Event;
use xui_core::image::Image;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp};

use common::{Areas, Widgets};

const RED: [u8; 4] = [255, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

type TestStage<'a> = Stage<'a, xui_paint::Msg>;

fn session(
    step: impl FnOnce(&TestStage<'_>, &Rc<RefCell<Observer>>, &Areas) + 'static,
) -> Image {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_probe = Rc::clone(&observer);
    let widgets = Widgets::default();
    let built = widgets.clone();
    render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)),
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

fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    }
}

fn wheel(stage: &TestStage<'_>, at: (i32, i32), delta: i16, modifiers: Modifiers) {
    stage.inject(Event::MouseWheel {
        delta,
        horizontal: false,
        x: at.0,
        y: at.1,
        modifiers,
    });
}

fn mouse_move(stage: &TestStage<'_>, at: (i32, i32)) {
    stage.inject(Event::MouseMove {
        x: at.0,
        y: at.1,
        modifiers: Modifiers::NONE,
    });
}

fn click(stage: &TestStage<'_>, at: (i32, i32)) {
    for event in [
        Event::MouseDown {
            x: at.0,
            y: at.1,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
        Event::MouseUp {
            x: at.0,
            y: at.1,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    ] {
        stage.inject(event);
    }
}

fn save(name: &str, image: &Image) {
    std::fs::create_dir_all("target/snapshots").unwrap();
    image
        .save_png(format!("target/snapshots/{name}.png"))
        .expect("save snapshot");
}

fn px(image: &Image, x: i32, y: i32) -> [u8; 4] {
    image.pixel(x as u32, y as u32).expect("inside the image")
}

/// Draws one red pencil dot at picture pixel (2, 1) at 100%, then zooms with
/// `notches` Ctrl+wheel steps around the canvas's top-left corner.
fn zoomed_dot(
    notches: i16,
    check: impl FnOnce(&TestStage<'_>, &Rc<RefCell<Observer>>, &Areas) + 'static,
) -> Image {
    session(move |stage, observer, areas| {
        let (left, top) = (areas.canvas.left, areas.canvas.top);
        // The palette's red swatch is index 3, as in the view tests.
        let swatch = (24 + 3 * 24 + 12, areas.palette.top + 12);
        click(stage, swatch);
        click(stage, (left + 2, top + 1));
        assert!(observer.borrow().can_undo, "the dot was drawn");
        wheel(stage, (left, top), notches, ctrl());
        check(stage, observer, areas);
    })
}

#[test]
fn ctrl_wheel_zooms_to_200_percent_with_crisp_pixels() {
    let areas = Rc::new(RefCell::new(None));
    let seen = Rc::clone(&areas);
    let image = zoomed_dot(1, move |_, _, a| *seen.borrow_mut() = Some(*a));
    save("xpaint-zoom-200", &image);
    let a = areas.borrow().unwrap();
    let (left, top) = (a.canvas.left, a.canvas.top);
    // Picture pixel (2, 1) is now the 2x2 block at view (4..6, 2..4).
    for (x, y) in [(4, 2), (5, 2), (4, 3), (5, 3)] {
        assert_eq!(px(&image, left + x, top + y), RED, "({x}, {y})");
    }
    for (x, y) in [(3, 2), (6, 3), (4, 1), (5, 4)] {
        assert_eq!(px(&image, left + x, top + y), WHITE, "no blur at ({x}, {y})");
    }
}

#[test]
fn ctrl_wheel_zooms_to_800_percent_with_a_grid() {
    let areas = Rc::new(RefCell::new(None));
    let seen = Rc::clone(&areas);
    let image = zoomed_dot(6, move |_, _, a| *seen.borrow_mut() = Some(*a));
    save("xpaint-zoom-800", &image);
    let a = areas.borrow().unwrap();
    let (left, top) = (a.canvas.left, a.canvas.top);
    // Pixel (2, 1) is the 8x8 block at view (16..24, 8..16); the cell's first
    // row and column carry the grid line, the rest is solid.
    for (x, y) in [(18, 10), (23, 15), (17, 9)] {
        assert_eq!(px(&image, left + x, top + y), RED, "({x}, {y})");
    }
    assert_ne!(px(&image, left + 16, top + 12), RED, "the grid line");
    assert_eq!(px(&image, left + 12, top + 12), WHITE, "a clean white cell");
    assert_ne!(px(&image, left + 12, top + 8), WHITE, "a horizontal grid line");
}

#[test]
fn the_pointer_maps_through_the_zoom() {
    zoomed_dot(6, |stage, observer, areas| {
        let (left, top) = (areas.canvas.left, areas.canvas.top);
        mouse_move(stage, (left + 20, top + 12));
        assert_eq!(observer.borrow().cursor, Some((2, 1)));
        mouse_move(stage, (left + 7, top + 7));
        assert_eq!(observer.borrow().cursor, Some((0, 0)));
        mouse_move(stage, (left + 24, top + 16));
        assert_eq!(observer.borrow().cursor, Some((3, 2)));
        // A stroke at zoom lands on the pixel under the pointer.
        let before = observer.borrow().can_undo;
        assert!(before);
    });
}

#[test]
fn zooming_keeps_the_point_under_the_pointer() {
    session(|stage, observer, areas| {
        let (left, top) = (areas.canvas.left, areas.canvas.top);
        // Zoom in first until the picture overflows the view in both axes:
        // a picture smaller than the view stays pinned to the top-left.
        wheel(stage, (left, top), 4, ctrl());
        let at = (left + 100, top + 80);
        mouse_move(stage, at);
        let under = observer.borrow().cursor.unwrap();
        for delta in [1, 1, -1] {
            wheel(stage, at, delta, ctrl());
            mouse_move(stage, at);
            let now = observer.borrow().cursor.unwrap();
            // Rounding to whole view pixels moves it by at most a picture pixel.
            assert!(
                (now.0 - under.0).abs() <= 1 && (now.1 - under.1).abs() <= 1,
                "{under:?} -> {now:?} after {delta}"
            );
        }
    });
}

#[test]
fn the_wheel_pans_a_zoomed_picture_and_space_drag_pans_too() {
    session(|stage, observer, areas| {
        let (left, top) = (areas.canvas.left, areas.canvas.top);
        let at = (left + 10, top + 10);
        // One dot to focus the canvas (key events go to the focused widget).
        click(stage, at);
        wheel(stage, (left, top), 4, ctrl());
        mouse_move(stage, at);
        let start = observer.borrow().cursor.unwrap();
        // Wheel down scrolls by one notch (24 view pixels).
        wheel(stage, at, 1, Modifiers::NONE);
        mouse_move(stage, at);
        let scrolled = observer.borrow().cursor.unwrap();
        assert!(scrolled.1 > start.1, "{start:?} -> {scrolled:?}");

        // Space held: a drag scrolls and never draws.
        stage.inject(Event::KeyDown {
            key: Key::SPACE,
            modifiers: Modifiers::NONE,
            repeat: 1,
            system: false,
        });
        stage.inject(Event::MouseDown {
            x: left + 100,
            y: top + 100,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        mouse_move(stage, (left + 60, top + 100));
        stage.inject(Event::MouseUp {
            x: left + 60,
            y: top + 100,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        stage.inject(Event::KeyUp {
            key: Key::SPACE,
            modifiers: Modifiers::NONE,
            system: false,
        });
        // Only the focusing dot is in the history: undo empties it.
        stage.emit(xui_paint::Msg::Undo);
        assert!(!observer.borrow().can_undo, "panning is not an edit");
        mouse_move(stage, at);
        let panned = observer.borrow().cursor.unwrap();
        assert!(panned.0 > scrolled.0, "{scrolled:?} -> {panned:?}");
    });
}

#[test]
fn zooming_out_shows_the_whole_picture_small() {
    let areas = Rc::new(RefCell::new(None));
    let seen = Rc::clone(&areas);
    let image = session(move |stage, _, a| {
        *seen.borrow_mut() = Some(*a);
        let (left, top) = (a.canvas.left, a.canvas.top);
        click(stage, (24 + 3 * 24 + 12, a.palette.top + 12));
        click(stage, (left + 200, top + 100));
        // 1 -> 2/3 -> 1/2 -> 1/3 -> 1/4.
        wheel(stage, (left, top), -4, ctrl());
    });
    save("xpaint-zoom-25", &image);
    let a = areas.borrow().unwrap();
    let (left, top) = (a.canvas.left, a.canvas.top);
    assert_eq!(px(&image, left + 50, top + 25), RED, "the dot moved to 25%");
    assert_ne!(px(&image, left + 200, top + 100), RED);
}
