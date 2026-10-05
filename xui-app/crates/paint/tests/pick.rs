//! The Picker (eyedropper) and the palette on the offscreen stage, which
//! delivers pointer events in node-local coordinates like a real backend.

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::Event;
use xui_core::message::{Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp};

use common::Widgets;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

fn press(stage: &Stage<'_, Msg>, x: i32, y: i32, button: MouseButton) {
    for event in [
        Event::MouseDown {
            x,
            y,
            button,
            modifiers: Modifiers::NONE,
        },
        Event::MouseUp {
            x,
            y,
            button,
            modifiers: Modifiers::NONE,
        },
    ] {
        stage.inject(event);
    }
}

#[test]
fn the_picker_samples_into_the_clicked_side_and_updates_the_swatch() {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let probe = Rc::clone(&observer);
    let widgets = Widgets::default();
    let built = widgets.clone();
    let palette_top = Rc::new(Cell::new(0));
    let seen_palette = Rc::clone(&palette_top);
    let image = render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)),
        move |ui| {
            built.record(PaintApp::build_observed(
                ui,
                Rc::new(MemoryStorage::new()),
                probe,
            ))
        },
        move |stage| {
            let areas = widgets.areas(stage.ui());
            seen_palette.set(areas.palette.top);
            let y = areas.canvas.top + 60;
            // Paint a red dot with the pencil at canvas (50, 60): red is
            // palette index 3.
            let red = (24 + 3 * 24 + 12, areas.palette.top + 12);
            press(stage, red.0, red.1, MouseButton::Left);
            assert_eq!(observer.borrow().primary, RED);
            press(stage, 50, y, MouseButton::Left);

            // Picker is the last tool cell (index 7).
            press(stage, 7 * 28 + 14, 15, MouseButton::Left);
            // Right button samples the dot into the secondary colour only.
            press(stage, 50, y, MouseButton::Right);
            assert_eq!(observer.borrow().secondary, RED);
            assert_eq!(observer.borrow().primary, RED);
            // Left button samples white background into the primary only.
            press(stage, 200, y, MouseButton::Left);
            assert_eq!(observer.borrow().primary, WHITE);
            assert_eq!(observer.borrow().secondary, RED);
            // Picking is not an edit beyond the one pencil dot.
            assert!(observer.borrow().can_undo);
            // Outside the bitmap nothing is sampled.
            press(stage, 500, y, MouseButton::Left);
            assert_eq!(observer.borrow().primary, WHITE);
        },
    )
    .expect("render");

    // The palette's secondary swatch (below the primary) now shows red.
    let swatch = image
        .pixel(10, (palette_top.get() + 18) as u32)
        .expect("the swatch is on screen");
    assert!(
        swatch[0] > 200 && swatch[1] < 60,
        "secondary swatch {swatch:?}"
    );
    let _ = BLACK;
}
