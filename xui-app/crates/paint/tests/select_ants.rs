//! The marching ants through the real window: snapshots at 100% and 400%.

#![allow(dead_code, unused_imports)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Bitmap;
use xui_paint::view::{Command, SystemClipboard, menu_item};

use common::driver::*;

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    ..Modifiers::NONE
};

/// Tool-grid cells of the P2 tools.
const RECT_SELECT: i32 = 8;
const ELLIPSE_SELECT: i32 = 9;
const LASSO: i32 = 10;
const WAND: i32 = 11;

fn drag_with(
    stage: &Stg<'_>,
    from: (i32, i32),
    to: (i32, i32),
    button: MouseButton,
    mods: Modifiers,
) {
    stage.inject(Event::MouseDown {
        x: from.0,
        y: from.1,
        button,
        modifiers: mods,
    });
    stage.inject(Event::MouseMove {
        x: to.0,
        y: to.1,
        modifiers: mods,
    });
    stage.inject(Event::MouseUp {
        x: to.0,
        y: to.1,
        button,
        modifiers: mods,
    });
}

/// Renders a black page with a rectangle and an ellipse selection and saves it.
fn ants_at(zoom_in: usize, name: &str) {
    let image = session(move |stage, _, parts| {
        let a = &parts.areas;
        ctrl(stage, Key::A);
        press(stage, Key::BACK);
        for _ in 0..zoom_in {
            stage.emit(Msg::Command(Command::ZoomIn));
        }
        // The picture is centred or scrolled; click through the real mapping.
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        let (cx, cy) = (
            a.canvas.left + a.canvas.width() / 2,
            a.canvas.top + a.canvas.height() / 2,
        );
        drag(
            stage,
            (cx - 60, cy - 40),
            (cx + 20, cy + 30),
            MouseButton::Left,
        );
        click(stage, tool_cell(a, ELLIPSE_SELECT), MouseButton::Left);
        drag_with(
            stage,
            (cx - 20, cy - 20),
            (cx + 70, cy + 50),
            MouseButton::Left,
            CTRL,
        );
    });
    std::fs::create_dir_all("target/snapshots").unwrap();
    image
        .save_png(format!("target/snapshots/{name}.png"))
        .unwrap();
    // The dashes are white on the black page: count the light pixels there.
    let areas = session_areas();
    let mut white = 0;
    for y in areas.canvas.top + 2..areas.canvas.bottom - 2 {
        for x in areas.canvas.left + 2..areas.canvas.right - 2 {
            // Anti-aliased one-pixel lines are light grey at worst.
            if image
                .pixel(x as u32, y as u32)
                .is_some_and(|p| p[..3].iter().all(|c| *c > 150))
            {
                white += 1;
            }
        }
    }
    assert!(
        white > 10,
        "{name}: {white} white ant pixels on the black page"
    );
}

#[test]
fn the_ants_draw_at_100_and_400_percent() {
    ants_at(0, "paint-ants-100");
    ants_at(3, "paint-ants-400");
}
