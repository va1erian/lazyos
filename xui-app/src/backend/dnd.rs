//! Drag and drop for xui apps over the compositor's protocol (issue #145,
//! `docs/archiver-plan.md`).
//!
//! xui has no drag-and-drop vocabulary, so this is a backend feature an app
//! opts into with two hooks:
//!
//! * [`LazyOSBackend::on_drag_gesture`]: when the pointer travels past
//!   [`THRESHOLD`] with the left button held since a press on a widget, the
//!   backend asks the hook what that widget drags. With a payload it offers
//!   it to `clipboardd`, calls `DragStart` with the token, and tells the
//!   pressed widget its capture is gone (the compositor owns the pointer
//!   until `DragEnded`).
//! * [`LazyOSBackend::on_drag_event`]: the window's drag events, with a
//!   drop's token already pasted (through `clipboardd`, with this task's own
//!   credentials, so a cross-session drop is refused there).
//!
//! The hooks run on the UI thread outside `App::update`; an app turns what
//! they report into messages with `Ui::proxy`, which the same loop tick
//! delivers.

use std::cell::{Cell, RefCell};

use xui_core::backend::{Event, WidgetId, WindowId};

use crate::display::DragEvent as WireEvent;
use crate::platform::clipboard;

use super::LazyOSBackend;

/// Design pixels a press must travel before it becomes a drag.
pub const THRESHOLD: i32 = 6;

/// What a drag carries: offered to `clipboardd` as `mime`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragOffer {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// A drag-and-drop event as an app sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropEvent {
    /// A drag carrying `mime` entered the window at `(x, y)` (window pixels).
    Enter { x: i32, y: i32, mime: String },
    /// It moved.
    Over { x: i32, y: i32 },
    /// It left (or was cancelled while over the window).
    Leave,
    /// It was dropped: the pasted payload, or the paste's negative errno.
    Drop {
        x: i32,
        y: i32,
        mime: String,
        data: Result<Vec<u8>, i64>,
    },
    /// A drag this window started was accepted by the compositor.
    Started,
    /// Starting a drag failed (the offer or `DragStart`), with the errno.
    Refused(i64),
    /// A drag this window started ended: dropped on a target, or not.
    Ended { dropped: bool },
}

type EventHook = Box<dyn Fn(WindowId, &DropEvent)>;
type GestureHook = Box<dyn Fn(WindowId, WidgetId, (i32, i32)) -> Option<DragOffer>>;

/// A left press that may become a drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Press {
    window: WindowId,
    widget: WidgetId,
    /// Window pixels, for the threshold.
    x: i32,
    y: i32,
    /// Where in the widget it was pressed, for the hook.
    local: (i32, i32),
}

/// The backend's drag-and-drop state.
#[derive(Default)]
pub(super) struct Dnd {
    on_event: RefCell<Option<EventHook>>,
    on_gesture: RefCell<Option<GestureHook>>,
    press: Cell<Option<Press>>,
    dragging: Cell<bool>,
}

/// Whether the pointer at `now` is past the drag threshold from `from`.
pub fn past_threshold(from: (i32, i32), now: (i32, i32), scale: u32) -> bool {
    let limit = THRESHOLD * scale.max(1) as i32;
    (now.0 - from.0).abs() > limit || (now.1 - from.1).abs() > limit
}

impl LazyOSBackend {
    /// Report this app's drag-and-drop events to `hook`.
    pub fn on_drag_event(&self, hook: impl Fn(WindowId, &DropEvent) + 'static) {
        *self.dnd.on_event.borrow_mut() = Some(Box::new(hook));
    }

    /// Ask `hook` what a press-and-drag on a widget carries (`None`: no
    /// drag); it gets the widget and where in it (widget pixels) the press
    /// was, so a list can refuse a drag that started on its header.
    pub fn on_drag_gesture(
        &self,
        hook: impl Fn(WindowId, WidgetId, (i32, i32)) -> Option<DragOffer> + 'static,
    ) {
        *self.dnd.on_gesture.borrow_mut() = Some(Box::new(hook));
    }

    /// Whether a drag this app started is in flight.
    pub fn is_dragging(&self) -> bool {
        self.dnd.dragging.get()
    }

    fn emit_drop_event(&self, window: WindowId, event: &DropEvent) {
        // The hook is taken out while it runs, so it may call back in.
        let hook = self.dnd.on_event.borrow_mut().take();
        if let Some(hook) = hook {
            hook(window, event);
            self.dnd.on_event.borrow_mut().get_or_insert(hook);
        }
    }

    /// A left press on `widget` at window pixels `(x, y)`, `local` inside it.
    pub(super) fn drag_press(
        &self,
        window: WindowId,
        widget: WidgetId,
        (x, y): (i32, i32),
        local: (i32, i32),
    ) {
        let armed = widget != WidgetId::NONE && self.dnd.on_gesture.borrow().is_some();
        self.dnd.press.set(armed.then_some(Press {
            window,
            widget,
            x,
            y,
            local,
        }));
    }

    /// The left button went up: no drag from this press.
    pub(super) fn drag_release(&self) {
        self.dnd.press.set(None);
    }

    /// The pointer moved with a press armed: start a drag past the threshold.
    pub(super) fn drag_motion(&self, window: WindowId, x: i32, y: i32) {
        let Some(press) = self.dnd.press.get() else {
            return;
        };
        if press.window != window || !past_threshold((press.x, press.y), (x, y), self.scale()) {
            return;
        }
        // One question per press, whatever the answer.
        self.dnd.press.set(None);
        let hook = self.dnd.on_gesture.borrow_mut().take();
        let offer = hook
            .as_ref()
            .and_then(|hook| hook(window, press.widget, press.local));
        if let Some(hook) = hook {
            self.dnd.on_gesture.borrow_mut().get_or_insert(hook);
        }
        if let Some(offer) = offer {
            self.start_drag(window, press, &offer);
        }
    }

    fn start_drag(&self, window: WindowId, press: Press, offer: &DragOffer) {
        let (Some(client), Some(surface)) = (self.display_client(), self.surface_of(window)) else {
            self.emit_drop_event(window, &DropEvent::Refused(-crate::sys::errno::EINVAL));
            return;
        };
        let started = clipboard::offer(&offer.mime, &offer.bytes)
            .and_then(|token| client.drag_start(surface, token, &offer.mime));
        match started {
            Ok(()) => {
                self.dnd.dragging.set(true);
                // The widget that took the press never sees its release (the
                // compositor owns the pointer now): end its capture.
                let captured = self.captured.take().unwrap_or(press.widget);
                self.deliver(window, captured, &Event::CaptureChanged);
                self.emit_drop_event(window, &DropEvent::Started);
            }
            Err(code) => self.emit_drop_event(window, &DropEvent::Refused(code)),
        }
    }

    /// A compositor drag event for `window`.
    pub(super) fn drag_message(&self, window: WindowId, event: WireEvent) {
        let event = match event {
            WireEvent::Enter { x, y, mime } => DropEvent::Enter { x, y, mime },
            WireEvent::Over { x, y } => DropEvent::Over { x, y },
            WireEvent::Leave => DropEvent::Leave,
            WireEvent::Drop { x, y, token, mime } => {
                let data = clipboard::paste(token, &mime);
                DropEvent::Drop { x, y, mime, data }
            }
            WireEvent::Ended { dropped } => {
                self.dnd.dragging.set(false);
                DropEvent::Ended { dropped }
            }
        };
        self.emit_drop_event(window, &event);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::test_support::client_backend;
    use super::*;

    #[test]
    fn the_threshold_scales_with_the_desktop() {
        assert!(!past_threshold((10, 10), (16, 16), 1));
        assert!(past_threshold((10, 10), (17, 10), 1));
        assert!(!past_threshold((10, 10), (22, 10), 2));
        assert!(past_threshold((10, 10), (10, -3), 2));
    }

    #[test]
    fn a_press_asks_the_hook_once_past_the_threshold() {
        let backend = client_backend();
        let asked = Rc::new(Cell::new(0));
        let seen = Rc::clone(&asked);
        backend.on_drag_gesture(move |_, widget, local| {
            assert_eq!((widget, local), (WidgetId::from_raw(7), (5, 6)));
            seen.set(seen.get() + 1);
            None
        });
        let window = WindowId::from_raw(1);
        backend.drag_press(window, WidgetId::from_raw(7), (50, 50), (5, 6));
        backend.drag_motion(window, 52, 52);
        assert_eq!(asked.get(), 0);
        backend.drag_motion(window, 70, 50);
        backend.drag_motion(window, 90, 50);
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_release_disarms_the_press() {
        let backend = client_backend();
        let asked = Rc::new(Cell::new(false));
        let seen = Rc::clone(&asked);
        backend.on_drag_gesture(move |_, _, _| {
            seen.set(true);
            None
        });
        let window = WindowId::from_raw(1);
        backend.drag_press(window, WidgetId::from_raw(3), (0, 0), (0, 0));
        backend.drag_release();
        backend.drag_motion(window, 100, 100);
        assert!(!asked.get());
    }

    #[test]
    fn without_a_gesture_hook_nothing_is_armed() {
        let backend = client_backend();
        backend.drag_press(WindowId::from_raw(1), WidgetId::from_raw(3), (0, 0), (0, 0));
        assert!(backend.dnd.press.get().is_none());
    }

    #[test]
    fn leave_and_ended_events_reach_the_hook() {
        let backend = client_backend();
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&events);
        backend.on_drag_event(move |_, event| log.borrow_mut().push(event.clone()));
        let window = WindowId::from_raw(1);
        backend.drag_message(
            window,
            WireEvent::Enter {
                x: 1,
                y: 2,
                mime: "text/uri-list".into(),
            },
        );
        backend.drag_message(window, WireEvent::Leave);
        backend.dnd.dragging.set(true);
        backend.drag_message(window, WireEvent::Ended { dropped: true });
        assert!(!backend.is_dragging());
        assert_eq!(
            *events.borrow(),
            [
                DropEvent::Enter {
                    x: 1,
                    y: 2,
                    mime: "text/uri-list".into()
                },
                DropEvent::Leave,
                DropEvent::Ended { dropped: true },
            ]
        );
    }
}
