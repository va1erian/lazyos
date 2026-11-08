//! The window's events and timers, turned into the Terminal's messages.

use xui_core::app::Ui;
use xui_core::backend::Event;
use xui_core::{Control, WidgetId};

use super::msg::Msg;
use super::timers::Timers;
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

/// Tell the app when the window's size changes.
pub(super) fn watch_resize(ui: &mut Ui<Msg>) {
    ui.register_events(WidgetId::NONE, |event| {
        matches!(event, Event::Resize { .. }).then_some(Msg::Resized)
    });
}

/// Route the timers: each has its own id, anything else is the poll tick.
pub(super) fn route_timers(ui: &mut Ui<Msg>, timers: &Timers) {
    let timers = timers.clone();
    ui.on_timer(move |id| Some(timers.message(id)));
}
