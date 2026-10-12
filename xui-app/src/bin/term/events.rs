//! The window's events and timers, turned into the Terminal's messages.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Event, TimerId};
use xui_core::Control;

use super::msg::Msg;
use super::pointer::{mouse, MouseKind};

/// Route the root node's keyboard, focus, wheel and pointer events.
pub(super) fn install(root: &Control<Msg>) {
    root.on_events(|event| match event {
        Event::Char(ch) => Some(Msg::Char(*ch)),
        Event::KeyDown { key, modifiers, .. } => Some(Msg::Key(*key, *modifiers)),
        Event::SetFocus => Some(Msg::Focus(true)),
        Event::KillFocus => Some(Msg::Focus(false)),
        Event::MouseWheel {
            delta,
            horizontal: false,
            x,
            y,
            modifiers,
        } => Some(Msg::Wheel(*delta, *modifiers, *x, *y)),
        Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        } => Some(mouse(MouseKind::Down(*button), *x, *y, *modifiers)),
        Event::MouseUp {
            x,
            y,
            button,
            modifiers,
        } => Some(mouse(MouseKind::Up(*button), *x, *y, *modifiers)),
        Event::MouseDoubleClick {
            x,
            y,
            button,
            modifiers,
        } => Some(mouse(MouseKind::Double(*button), *x, *y, *modifiers)),
        Event::MouseMove { x, y, modifiers } => {
            Some(mouse(MouseKind::Move, *x, *y, *modifiers))
        }
        _ => None,
    });
}

/// Tell the timers apart: the autoscroll, the input retry and the blink each
/// have an id of their own; anything else is the poll tick.
pub(super) fn route_timers(
    ui: &mut Ui<Msg>,
    auto: &Rc<Cell<Option<TimerId>>>,
    flush: &Rc<Cell<Option<TimerId>>>,
    blink: &Rc<Cell<Option<TimerId>>>,
) {
    let timers = (Rc::clone(auto), Rc::clone(flush), Rc::clone(blink));
    ui.on_timer(move |id| {
        Some(if timers.0.get() == Some(id) {
            Msg::Autoscroll
        } else if timers.1.get() == Some(id) {
            Msg::Flush
        } else if timers.2.get() == Some(id) {
            Msg::Blink
        } else {
            Msg::Tick
        })
    });
}
