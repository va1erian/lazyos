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
use crate::model::Side;

/// Maps which side a button paints.
pub(super) fn side_of(button: MouseButton) -> Option<Side> {
    match button {
        MouseButton::Left => Some(Side::Primary),
        MouseButton::Right => Some(Side::Secondary),
        _ => None,
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
            x, y, button, ..
        } => {
            let panning = state.borrow().space || *button == MouseButton::Middle;
            if panning {
                state.borrow_mut().pan = Some((*x, *y));
                ui.focus(id);
                ui.set_capture(id);
                return None;
            }
            let side = side_of(*button)?;
            ui.focus(id);
            ui.set_capture(id);
            let (cx, cy) = state.borrow().to_doc(*x, *y);
            Some(Msg::Canvas(CanvasMsg::Down { x: cx, y: cy, side }))
        }
        Event::MouseMove { x, y, .. } => {
            if pan_to(ui, id, state, (*x, *y)) {
                return None;
            }
            let (cx, cy) = {
                let mut state = state.borrow_mut();
                let at = state.to_doc(*x, *y);
                state.cursor = Some(at);
                at
            };
            ui.invalidate(id);
            Some(Msg::Canvas(CanvasMsg::Move { x: cx, y: cy }))
        }
        Event::MouseUp { x, y, .. } => {
            let was_panning = state.borrow_mut().pan.take().is_some();
            let at = state.borrow().to_doc(*x, *y);
            // xui gap: G2 — release_capture raises a synchronous CaptureChanged
            // that must not read as a fresh interruption.
            releasing.set(true);
            ui.release_capture();
            releasing.set(false);
            if was_panning {
                return None;
            }
            Some(Msg::Canvas(CanvasMsg::Up { x: at.0, y: at.1 }))
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
                if matches!(event, Event::KillFocus) {
                    state.space = false;
                }
            }
            ui.invalidate(id);
            Some(Msg::Canvas(CanvasMsg::Cancel))
        }
        Event::KeyDown { key, .. } if *key == Key::SPACE => {
            state.borrow_mut().space = true;
            None
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
            wheel(ui, id, state, *delta, *horizontal, (*x, *y), *modifiers);
            None
        }
        _ => None,
    }
}

/// Continues a pan drag; returns whether one is in progress.
fn pan_to(ui: &Ui<Msg>, id: WidgetId, state: &Rc<RefCell<CanvasState>>, at: (i32, i32)) -> bool {
    let moved = {
        let mut state = state.borrow_mut();
        let Some(last) = state.pan else {
            return false;
        };
        state.pan = Some(at);
        state.change_viewport(|vp, doc, view| {
            vp.pan_by(last.0 - at.0, last.1 - at.1, doc, view);
        })
    };
    if moved {
        ui.invalidate(id);
    }
    true
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
) {
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
}
