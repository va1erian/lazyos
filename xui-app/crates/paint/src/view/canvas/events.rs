#![forbid(unsafe_code)]

//! The canvas event mapper: pointer input to picture coordinates, the wheel to
//! scrolling and zooming, Space and the middle button to panning.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Event, WidgetId};
use xui_core::message::{Key, Modifiers, MouseButton};

use super::state::CanvasState;
use super::{CanvasMsg, Msg};
use crate::model::{Mods, Side, TextKey, Tool};

/// Maps which side a button paints.
pub(super) fn side_of(button: MouseButton) -> Option<Side> {
    match button {
        MouseButton::Left => Some(Side::Primary),
        MouseButton::Right => Some(Side::Secondary),
        _ => None,
    }
}

/// The pointer modifiers the model reads.
fn mods_of(modifiers: Modifiers) -> Mods {
    Mods {
        ctrl: modifiers.ctrl || modifiers.win,
        shift: modifiers.shift,
        alt: modifiers.alt,
    }
}

/// Pixels scrolled per wheel notch.
const WHEEL_STEP: i32 = 24;

pub(super) fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<CanvasState>>,
    releasing: &Rc<Cell<bool>>,
    event: &Event,
) -> Option<Msg> {
    match event {
        Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        } => {
            let panning = {
                let state = state.borrow();
                state.space
                    || *button == MouseButton::Middle
                    || (state.tool == Tool::Pan && *button == MouseButton::Left)
            };
            if panning {
                state.borrow_mut().pan = Some((*x, *y));
                ui.focus(id);
                ui.set_capture(id);
                return None;
            }
            let side = side_of(*button)?;
            ui.focus(id);
            ui.set_capture(id);
            let (cx, cy) = {
                let mut state = state.borrow_mut();
                state.dragging = true;
                state.to_doc(*x, *y)
            };
            Some(Msg::Canvas(CanvasMsg::Down {
                x: cx,
                y: cy,
                side,
                mods: mods_of(*modifiers),
            }))
        }
        Event::MouseMove { x, y, modifiers } => {
            if let Some(moved) = pan_to(ui, id, state, (*x, *y)) {
                return moved.then_some(Msg::Canvas(CanvasMsg::View));
            }
            let (cx, cy) = {
                let mut state = state.borrow_mut();
                let at = state.to_doc(*x, *y);
                state.cursor = Some(at);
                at
            };
            ui.invalidate(id);
            Some(Msg::Canvas(CanvasMsg::Move {
                x: cx,
                y: cy,
                mods: mods_of(*modifiers),
            }))
        }
        Event::MouseUp {
            x, y, modifiers, ..
        } => {
            let was_panning = state.borrow_mut().pan.take().is_some();
            let at = {
                let mut state = state.borrow_mut();
                state.dragging = false;
                state.to_doc(*x, *y)
            };
            // xui gap: G2 — release_capture raises a synchronous CaptureChanged
            // that must not read as a fresh interruption.
            releasing.set(true);
            ui.release_capture();
            releasing.set(false);
            if was_panning {
                return None;
            }
            Some(Msg::Canvas(CanvasMsg::Up {
                x: at.0,
                y: at.1,
                mods: mods_of(*modifiers),
            }))
        }
        Event::MouseLeave => {
            // Only hide the brush ring: with capture held, a leave does not end
            // a drag (a source that emits one anyway must not cancel it).
            let had = state.borrow_mut().cursor.take().is_some();
            if had {
                ui.invalidate(id);
            }
            None
        }
        Event::CaptureChanged | Event::KillFocus => {
            if releasing.get() {
                return None;
            }
            {
                let mut state = state.borrow_mut();
                state.cursor = None;
                state.pan = None;
                state.dragging = false;
                if matches!(event, Event::KillFocus) {
                    state.space = false;
                }
            }
            ui.invalidate(id);
            Some(Msg::Canvas(CanvasMsg::Cancel))
        }
        Event::KeyDown { key, modifiers, .. } => key_down(ui, id, state, releasing, *key, *modifiers),
        Event::Char(c) => {
            let typing = state.borrow().editing && !c.is_control();
            typing.then_some(Msg::Canvas(CanvasMsg::Char(*c)))
        }
        Event::KeyUp { key, .. } if *key == Key::SPACE => {
            state.borrow_mut().space = false;
            None
        }
        Event::MouseWheel {
            delta,
            horizontal,
            x,
            y,
            modifiers,
        } => {
            let moved = wheel(ui, id, state, *delta, *horizontal, (*x, *y), *modifiers);
            moved.then_some(Msg::Canvas(CanvasMsg::View))
        }
        _ => None,
    }
}

/// The text-editing key a press means.
fn text_key(key: Key, modifiers: Modifiers) -> Option<TextKey> {
    let plain = !modifiers.ctrl && !modifiers.alt && !modifiers.win;
    match key {
        Key::RETURN if modifiers.ctrl => Some(TextKey::Commit),
        Key::RETURN if plain => Some(TextKey::Enter),
        Key::ESCAPE => Some(TextKey::Cancel),
        Key::LEFT if plain => Some(TextKey::Left),
        Key::RIGHT if plain => Some(TextKey::Right),
        Key::UP if plain => Some(TextKey::Up),
        Key::DOWN if plain => Some(TextKey::Down),
        Key::HOME if plain => Some(TextKey::Home),
        Key::END if plain => Some(TextKey::End),
        Key::BACK if plain => Some(TextKey::Backspace),
        Key::DELETE if plain => Some(TextKey::Delete),
        _ => None,
    }
}

/// A key press on the canvas: Space starts a pan, a text block takes its
/// editing keys, and Esc during a drag takes the stroke back.
fn key_down(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<CanvasState>>,
    releasing: &Rc<Cell<bool>>,
    key: Key,
    modifiers: Modifiers,
) -> Option<Msg> {
    if key == Key::SPACE && !state.borrow().editing {
        state.borrow_mut().space = true;
        return None;
    }
    let (editing, dragging) = {
        let state = state.borrow();
        (state.editing, state.dragging)
    };
    if dragging && key == Key::ESCAPE {
        {
            let mut state = state.borrow_mut();
            state.dragging = false;
            state.pan = None;
        }
        // xui gap: G2 — as on a button release.
        releasing.set(true);
        ui.release_capture();
        releasing.set(false);
        ui.invalidate(id);
        return Some(Msg::Canvas(CanvasMsg::Abort));
    }
    if editing {
        return text_key(key, modifiers).map(|key| Msg::Canvas(CanvasMsg::Text(key)));
    }
    None
}

/// Continues a pan drag: `None` when none is in progress, else whether the
/// view moved.
fn pan_to(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<CanvasState>>,
    at: (i32, i32),
) -> Option<bool> {
    let moved = {
        let mut state = state.borrow_mut();
        let last = state.pan?;
        state.pan = Some(at);
        state.change_viewport(|vp, doc, view| {
            vp.pan_by(last.0 - at.0, last.1 - at.1, doc, view);
        })
    };
    if moved {
        ui.invalidate(id);
    }
    Some(moved)
}

/// Ctrl+wheel zooms around the pointer; the wheel scrolls (Shift, or a
/// horizontal wheel, sideways).
fn wheel(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<CanvasState>>,
    delta: i16,
    horizontal: bool,
    at: (i32, i32),
    modifiers: Modifiers,
) -> bool {
    let moved = {
        let mut state = state.borrow_mut();
        let moved = if modifiers.ctrl {
            state.change_viewport(|vp, doc, view| {
                for _ in 0..delta.unsigned_abs().min(8) {
                    if delta > 0 {
                        vp.zoom_in(at, doc, view);
                    } else {
                        vp.zoom_out(at, doc, view);
                    }
                }
            })
        } else {
            let step = i32::from(delta) * WHEEL_STEP;
            let (dx, dy) = if horizontal || modifiers.shift {
                (step, 0)
            } else {
                (0, step)
            };
            state.change_viewport(|vp, doc, view| {
                vp.pan_by(dx, dy, doc, view);
            })
        };
        // The pointer now points at a different picture pixel.
        if moved && state.cursor.is_some() {
            state.cursor = Some(state.to_doc(at.0, at.1));
        }
        moved
    };
    if moved {
        ui.invalidate(id);
    }
    moved
}
