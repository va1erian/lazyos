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
use xui_paint::viewport::Viewport;

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

/// Draws one red pencil dot at `DOT` (the middle of the picture) at 100%, then zooms with
/// `notches` Ctrl+wheel steps around the canvas's top-left corner.

/// The viewport after `notches` Ctrl+wheel steps around the picture's top-left
/// pixel `around`, worked out with the same arithmetic the canvas uses.
/// The dot's picture pixel: zooming around the middle of a picture that fits
/// the canvas keeps it under the pointer all the way.
const DOT: (i32, i32) = (160, 120);

fn viewport_after(areas: &Areas, notches: i16, around: (i32, i32)) -> Viewport {
    let doc = (320, 240);
    let view = (areas.canvas.width(), areas.canvas.height());
    let mut viewport = Viewport::new();
    viewport.clamp(doc, view);
    let (wx, wy) = areas.pixel(around.0, around.1);
    let anchor = (wx - areas.canvas.left, wy - areas.canvas.top);
    for _ in 0..notches.unsigned_abs() {
        if notches > 0 {
            viewport.zoom_in(anchor, doc, view);
        } else {
            viewport.zoom_out(anchor, doc, view);
        }
    }
    viewport
}

/// Draws one red pencil dot at `DOT` (the middle of the picture) at 100%, then zooms with
/// `notches` Ctrl+wheel steps around the dot.
fn zoomed_dot(
    notches: i16,
    check: impl FnOnce(&TestStage<'_>, &Rc<RefCell<Observer>>, &Areas, Viewport) + 'static,
) -> Image {
    session(move |stage, observer, areas| {
        // The palette's red swatch is index 3, as in the view tests.
        let swatch = (24 + 3 * 24 + 12, areas.palette.top + 12);
        click(stage, swatch);
        click(stage, areas.pixel(DOT.0, DOT.1));
        assert!(observer.borrow().can_undo, "the dot was drawn");
        wheel(stage, areas.pixel(DOT.0, DOT.1), notches, ctrl());
        check(stage, observer, areas, viewport_after(areas, notches, DOT));
    })
}

/// The window position of the top-left of picture pixel `(x, y)`.
fn at(areas: &Areas, viewport: &Viewport, x: i32, y: i32) -> (i32, i32) {
    let (vx, vy) = viewport.to_view(x, y);
    (areas.canvas.left + vx, areas.canvas.top + vy)
}

#[test]
fn ctrl_wheel_zooms_to_200_percent_with_crisp_pixels() {
    let seen = Rc::new(RefCell::new(None));
    let probe = Rc::clone(&seen);
    let image = zoomed_dot(1, move |_, _, areas, viewport| {
        *probe.borrow_mut() = Some((*areas, viewport));
    });
    save("xpaint-zoom-200", &image);
    let (areas, viewport) = seen.borrow().unwrap();
    assert_eq!(viewport.percent(), 200);
    // The dot's pixel is a 2 x 2 block, edges exactly on pixel borders.
    let (x, y) = at(&areas, &viewport, DOT.0, DOT.1);
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(px(&image, x + dx, y + dy), RED, "({dx}, {dy})");
    }
    for (dx, dy) in [(-1, 0), (2, 1), (0, -1), (1, 2)] {
        assert_eq!(px(&image, x + dx, y + dy), WHITE, "no blur at ({dx}, {dy})");
    }
    // The picture fits the canvas across (640 < 800): centred there.
    assert!(viewport.origin().0 < 0);
}

#[test]
fn ctrl_wheel_zooms_to_800_percent_with_a_grid() {
    let seen = Rc::new(RefCell::new(None));
    let probe = Rc::clone(&seen);
    let image = zoomed_dot(6, move |_, _, areas, viewport| {
        *probe.borrow_mut() = Some((*areas, viewport));
    });
    save("xpaint-zoom-800", &image);
    let (areas, viewport) = seen.borrow().unwrap();
    assert_eq!(viewport.percent(), 800);
    // The dot is an 8 x 8 block; the first row and column of every cell
    // carry the grid line, the rest is solid.
    let (x, y) = at(&areas, &viewport, DOT.0, DOT.1);
    for (dx, dy) in [(2, 2), (7, 7), (1, 1)] {
        assert_eq!(px(&image, x + dx, y + dy), RED, "({dx}, {dy})");
    }
    assert_ne!(px(&image, x, y + 4), RED, "the grid line");
    let (cx, cy) = at(&areas, &viewport, DOT.0 - 2, DOT.1 - 2);
    assert_eq!(px(&image, cx + 4, cy + 4), WHITE, "a clean white cell");
    assert_ne!(px(&image, cx + 4, cy), WHITE, "a horizontal grid line");
}

#[test]
fn the_pointer_maps_through_the_zoom() {
    zoomed_dot(6, |stage, observer, areas, viewport| {
        let hover = |dx: i32, dy: i32, x: i32, y: i32| {
            let (wx, wy) = at(areas, &viewport, x, y);
            mouse_move(stage, (wx + dx, wy + dy));
            observer.borrow().cursor
        };
        assert_eq!(hover(4, 4, DOT.0, DOT.1), Some(DOT));
        assert_eq!(hover(4, 4, DOT.0 - 2, DOT.1 - 1), Some((DOT.0 - 2, DOT.1 - 1)));
        assert_eq!(hover(7, 7, DOT.0 + 1, DOT.1 + 1), Some((DOT.0 + 1, DOT.1 + 1)));
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
    let seen = Rc::new(RefCell::new(None));
    let probe = Rc::clone(&seen);
    let image = session(move |stage, _, a| {
        click(stage, (24 + 3 * 24 + 12, a.palette.top + 12));
        click(stage, a.pixel(200, 100));
        // 1 -> 2/3 -> 1/2 -> 1/3 -> 1/4.
        wheel(stage, a.pixel(200, 100), -4, ctrl());
        *probe.borrow_mut() = Some((*a, viewport_after(a, -4, (200, 100))));
    });
    save("xpaint-zoom-25", &image);
    let (areas, viewport) = seen.borrow().unwrap();
    assert_eq!(viewport.percent(), 25);
    let (x, y) = at(&areas, &viewport, 200, 100);
    assert_eq!(px(&image, x, y), RED, "the dot moved to 25%");
    // The whole 320 x 240 picture is 80 x 60 and centred.
    let (x0, y0) = at(&areas, &viewport, 0, 0);
    assert_eq!(px(&image, x0 + 10, y0 + 10), WHITE);
    assert_ne!(px(&image, x0 - 4, y0 + 10), WHITE, "outside it is the background");
}

#[test]
fn the_zoom_shows_in_the_status_bar_and_the_observer() {
    session(|stage, observer, areas| {
        assert_eq!(observer.borrow().zoom_percent, 100);
        assert_eq!(observer.borrow().status[4], "100%");
        wheel(stage, areas.pixel(160, 120), 1, ctrl());
        assert_eq!(observer.borrow().zoom_percent, 200);
        assert_eq!(observer.borrow().status[4], "200%");
        wheel(stage, areas.pixel(160, 120), -2, ctrl());
        assert_eq!(observer.borrow().status[4], "67%");
    });
}

#[test]
fn the_eraser_leaves_a_checkerboard() {
    let seen = Rc::new(RefCell::new(None));
    let probe = Rc::clone(&seen);
    let image = session(move |stage, _, areas| {
        *probe.borrow_mut() = Some(*areas);
        click(stage, (2 * 28 + 14, 15)); // Eraser
        click(stage, (12 * 28 + 14, 15)); // 16 px
        let from = areas.pixel(100, 100);
        let to = areas.pixel(200, 100);
        for event in [
            Event::MouseDown {
                x: from.0,
                y: from.1,
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            },
            Event::MouseMove {
                x: to.0,
                y: to.1,
                modifiers: Modifiers::NONE,
            },
            Event::MouseUp {
                x: to.0,
                y: to.1,
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            },
        ] {
            stage.inject(event);
        }
    });
    save("xpaint-eraser-checker", &image);
    let areas = seen.borrow().unwrap();
    let (y, left) = (areas.pixel(0, 100).1, areas.pixel(100, 0).0);
    let row: Vec<[u8; 4]> = (left + 4..left + 96).map(|x| px(&image, x, y)).collect();
    assert!(row.contains(&[255, 255, 255, 255]), "light checker cells");
    assert!(row.contains(&[204, 204, 204, 255]), "dark checker cells");
    // Away from the stroke the picture is still plain white.
    let (wx, wy) = areas.pixel(100, 20);
    assert_eq!(px(&image, wx + 10, wy), WHITE);
}
