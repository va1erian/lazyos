//! The Picker (eyedropper) and the palette on the offscreen stage, which
//! delivers pointer events in node-local coordinates like a real backend.

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Tool;
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
    let secondary_swatch = Rc::new(Cell::new((0, 0)));
    let seen_swatch = Rc::clone(&secondary_swatch);
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
            // The part of the secondary swatch the primary does not cover: the
            // panel's 8 px padding, the swatch offset by 14 and 30 wide (96 dpi).
            seen_swatch.set((
                areas.palette.left + 8 + 14 + 27,
                areas.palette.top + 8 + 14 + 27,
            ));
            // The dot at canvas (50, 60), and a spot of plain background.
            let dot = areas.canvas_at(50, 60);
            let background = areas.canvas_at(200, 60);
            let outside = areas.canvas_at(400, 60);
            // Paint a red dot with the pencil: red is palette index 3, in the
            // grid under the 60 px header (8 px padding, 18 px cells).
            let red = (
                areas.palette.left + 8 + 3 * 18 + 9,
                areas.palette.top + 60 + 9,
            );
            press(stage, red.0, red.1, MouseButton::Left);
            assert_eq!(observer.borrow().primary, RED);
            press(stage, dot.0, dot.1, MouseButton::Left);

            // K selects the Picker.
            stage.inject(Event::KeyDown {
                key: Key::K,
                modifiers: Modifiers::NONE,
                repeat: 1,
                system: false,
            });
            assert_eq!(observer.borrow().tool, Tool::Picker);
            // Right button samples the dot into the secondary colour only.
            press(stage, dot.0, dot.1, MouseButton::Right);
            assert_eq!(observer.borrow().secondary, RED);
            assert_eq!(observer.borrow().primary, RED);
            // Left button samples white background into the primary only.
            press(stage, background.0, background.1, MouseButton::Left);
            assert_eq!(observer.borrow().primary, WHITE);
            assert_eq!(observer.borrow().secondary, RED);
            // Picking is not an edit beyond the one pencil dot.
            assert!(observer.borrow().can_undo);
            // Outside the bitmap nothing is sampled.
            press(stage, outside.0, outside.1, MouseButton::Left);
            assert_eq!(observer.borrow().primary, WHITE);
        },
    )
    .expect("render");

    // The panel's secondary swatch (behind the primary) now shows red.
    let (x, y) = secondary_swatch.get();
    let swatch = image
        .pixel(x as u32, y as u32)
        .expect("the swatch is on screen");
    assert!(
        swatch[0] > 200 && swatch[1] < 60,
        "secondary swatch {swatch:?}"
    );
    let _ = BLACK;
}
