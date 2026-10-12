//! The shared driver of the view tests: where the panels sit at 96 dpi, mouse and
//! key helpers over the offscreen `Stage`, and `session`, which builds the app
//! with an observer and hands the step the placed widgets.

#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::{Event, WidgetId};
use xui_core::image::Image;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_core::widget::{ColorPanel, Menu};
use xui_paint::Msg;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp};

use super::{Areas, Widgets};

pub const RED: [u8; 4] = [255, 0, 0, 255];
pub const BLUE: [u8; 4] = [0, 0, 255, 255];
pub const BLACK: [u8; 4] = [0, 0, 0, 255];
pub const WHITE: [u8; 4] = [255, 255, 255, 255];

pub const WIDTH: i32 = 800;
pub const HEIGHT: i32 = 600;

/// The tool grid's cell and margin at the test DPI (96).
pub const TOOL_CELL: i32 = 32;
pub const TOOL_MARGIN: i32 = 4;
/// The Colors panel at the test DPI: padding, swatch, offset, button, cell.
pub const PAD: i32 = 8;
pub const SWATCH: i32 = 30;
pub const OFFSET: i32 = 14;
pub const BUTTON: i32 = 22;
pub const CELL: i32 = 18;

pub type Stg<'a> = Stage<'a, Msg>;

/// The centre of tool `index` in the grid (two to a row).
pub fn tool_cell(areas: &Areas, index: i32) -> (i32, i32) {
    (
        areas.toolbar.left + TOOL_MARGIN + (index % 2) * TOOL_CELL + TOOL_CELL / 2,
        areas.toolbar.top + TOOL_MARGIN + (index / 2) * TOOL_CELL + TOOL_CELL / 2,
    )
}

/// The centre of palette colour `index` (eight to a row, under the header).
pub fn swatch(areas: &Areas, index: i32) -> (i32, i32) {
    let header = PAD + OFFSET + SWATCH + PAD;
    (
        areas.palette.left + PAD + (index % 8) * CELL + CELL / 2,
        areas.palette.top + header + (index / 8) * CELL + CELL / 2,
    )
}

/// The middle of the primary swatch, which is in front.
pub fn primary_swatch(areas: &Areas) -> (i32, i32) {
    (
        areas.palette.left + PAD + SWATCH / 2,
        areas.palette.top + PAD + SWATCH / 2,
    )
}

/// A point of the secondary swatch the primary does not cover.
pub fn secondary_swatch(areas: &Areas) -> (i32, i32) {
    (
        areas.palette.left + PAD + OFFSET + SWATCH - 3,
        areas.palette.top + PAD + OFFSET + SWATCH - 3,
    )
}

/// The swap (`row` 0) or reset (`row` 1) button.
pub fn button(areas: &Areas, row: i32) -> (i32, i32) {
    (
        areas.palette.left + PAD + OFFSET + SWATCH + PAD + BUTTON / 2,
        areas.palette.top + PAD + row * (BUTTON + PAD / 2) + BUTTON / 2,
    )
}

pub fn down(x: i32, y: i32, button: MouseButton) -> Event {
    Event::MouseDown {
        x,
        y,
        button,
        modifiers: Modifiers::NONE,
    }
}

pub fn up(x: i32, y: i32, button: MouseButton) -> Event {
    Event::MouseUp {
        x,
        y,
        button,
        modifiers: Modifiers::NONE,
    }
}

pub fn click(stage: &Stg<'_>, at: (i32, i32), button: MouseButton) {
    stage.inject(down(at.0, at.1, button));
    stage.inject(up(at.0, at.1, button));
}

pub fn move_to(stage: &Stg<'_>, at: (i32, i32)) {
    stage.inject(Event::MouseMove {
        x: at.0,
        y: at.1,
        modifiers: Modifiers::NONE,
    });
}

/// Injects a drag from window `from` to `to` with `button`.
pub fn drag(stage: &Stg<'_>, from: (i32, i32), to: (i32, i32), button: MouseButton) {
    stage.inject(down(from.0, from.1, button));
    move_to(stage, to);
    stage.inject(up(to.0, to.1, button));
}

/// A key press with `modifiers`, to the focused widget (the canvas at start).
pub fn press_with(stage: &Stg<'_>, key: Key, modifiers: Modifiers) {
    stage.inject(Event::KeyDown {
        key,
        modifiers,
        repeat: 1,
        system: false,
    });
}

pub fn press(stage: &Stg<'_>, key: Key) {
    press_with(stage, key, Modifiers::NONE);
}

pub fn ctrl(stage: &Stg<'_>, key: Key) {
    let modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    press_with(stage, key, modifiers);
}

pub fn ctrl_shift(stage: &Stg<'_>, key: Key) {
    let modifiers = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::NONE
    };
    press_with(stage, key, modifiers);
}

/// What a step closure can read back besides the observer.
pub struct Parts {
    pub areas: Areas,
    pub menu: Rc<Menu<Msg>>,
    pub panel: Rc<ColorPanel<Msg>>,
    pub canvas: WidgetId,
}

/// Builds the app with an observer and runs `step` with the placed widgets.
pub fn session(step: impl FnOnce(&Stg<'_>, &Rc<RefCell<Observer>>, &Parts) + 'static) -> Image {
    session_with(None, step)
}

/// Like [`session`], with a system clipboard wired when one is given.
pub fn session_with(
    system: Option<Rc<dyn xui_paint::view::SystemClipboard>>,
    step: impl FnOnce(&Stg<'_>, &Rc<RefCell<Observer>>, &Parts) + 'static,
) -> Image {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_probe = Rc::clone(&observer);
    let widgets = Widgets::default();
    let built = widgets.clone();
    type Handles = (Rc<Menu<Msg>>, Rc<ColorPanel<Msg>>, WidgetId);
    let handles: Rc<RefCell<Option<Handles>>> = Rc::new(RefCell::new(None));
    let stored = Rc::clone(&handles);
    render_with(
        Snapshot::new(Dip(WIDTH as f32), Dip(HEIGHT as f32)),
        move |ui| {
            let mut app = PaintApp::build_observed(ui, Rc::new(MemoryStorage::new()), build_probe);
            if let (Ok(app), Some(system)) = (&mut app, system) {
                app.set_system_clipboard(system);
            }
            if let Ok(app) = &app {
                *stored.borrow_mut() = Some((app.menu(), app.color_panel(), app.canvas().id()));
            }
            built.record(app)
        },
        move |stage| {
            let (menu, panel, canvas) = handles.borrow_mut().take().expect("built");
            let parts = Parts {
                areas: widgets.areas(stage.ui()),
                menu,
                panel,
                canvas,
            };
            step(stage, &observer, &parts)
        },
    )
    .expect("render")
}

pub fn is(bitmap: &Image, x: u32, y: u32, color: [u8; 4]) -> bool {
    bitmap.pixel(x, y).is_some_and(|pixel| {
        pixel[0].abs_diff(color[0]) < 8
            && pixel[1].abs_diff(color[1]) < 8
            && pixel[2].abs_diff(color[2]) < 8
    })
}

pub fn status(observer: &Rc<RefCell<Observer>>, part: usize) -> String {
    observer.borrow().status[part].clone()
}

/// The placed areas of a fresh app, for reading pixels off a captured image.
pub fn session_areas() -> Areas {
    let slot: Rc<RefCell<Option<Areas>>> = Rc::new(RefCell::new(None));
    let keep = Rc::clone(&slot);
    session(move |_, _, parts| *keep.borrow_mut() = Some(parts.areas));
    let areas = *slot.borrow();
    areas.expect("the step ran")
}
