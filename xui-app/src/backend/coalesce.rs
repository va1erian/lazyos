//! Merging the pointer events of one input batch.
//!
//! The loop drains every queued input message, then presents once. A widget
//! that takes long to react to a move (a scroll bar's thumb drag re-lays a
//! page of text out) would then fall behind a pointer that moves faster than
//! it answers: the backlog is worked off before anything is painted, so the
//! content and the thumb show nothing until the button is released. Only the
//! last of several consecutive moves matters (a move carries a position, not
//! a delta), so a batch is cut to what a person would see; every other event
//! keeps its place and flushes what is held, so an order such as move, press,
//! move, release is intact. Wheel rolls are never merged: `xui`'s widgets
//! scroll one step per `MouseWheel` event whatever its delta (a summed roll
//! would lose notches), and a step is cheap.

use xui_core::backend::WindowId;

use crate::display::Event as DisplayEvent;

use super::LazyOSBackend;

/// The pointer event held back until something else arrives.
#[derive(Default)]
pub(super) struct Coalescer {
    held: Option<DisplayEvent>,
}

/// `held` and `next` as one event when they merge.
fn merged(held: DisplayEvent, next: DisplayEvent) -> Option<DisplayEvent> {
    match (held, next) {
        (DisplayEvent::PointerMove { .. }, DisplayEvent::PointerMove { x, y }) => {
            Some(DisplayEvent::PointerMove { x, y })
        }
        _ => None,
    }
}

impl Coalescer {
    /// Route `event`, holding it back when it is a move.
    pub(super) fn push(&mut self, backend: &LazyOSBackend, window: WindowId, event: DisplayEvent) {
        let holdable = matches!(event, DisplayEvent::PointerMove { .. });
        if let Some(held) = self.held.take() {
            match merged(held, event) {
                Some(both) => {
                    self.held = Some(both);
                    return;
                }
                None => backend.route_client_event(window, held),
            }
        }
        if holdable {
            self.held = Some(event);
        } else {
            backend.route_client_event(window, event);
        }
    }

    /// Route what is held. Called before anything that is not a pointer
    /// event is handled and when the batch ends.
    pub(super) fn flush(&mut self, backend: &LazyOSBackend, window: WindowId) {
        if let Some(held) = self.held.take() {
            backend.route_client_event(window, held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(x: i32, y: i32) -> DisplayEvent {
        DisplayEvent::PointerMove { x, y }
    }

    fn wheel(x: i32, y: i32, delta: i32) -> DisplayEvent {
        DisplayEvent::PointerWheel { x, y, delta }
    }

    #[test]
    fn consecutive_moves_keep_the_last() {
        assert_eq!(merged(mv(1, 1), mv(5, 6)), Some(mv(5, 6)));
    }

    #[test]
    fn rolls_are_never_merged() {
        assert_eq!(merged(wheel(3, 4, 1), wheel(3, 4, 2)), None);
        assert_eq!(merged(wheel(3, 4, -1), wheel(3, 4, 1)), None);
    }

    #[test]
    fn a_move_and_a_roll_never_merge() {
        assert_eq!(merged(mv(1, 1), wheel(1, 1, 1)), None);
        assert_eq!(merged(wheel(1, 1, 1), mv(1, 1)), None);
    }

    #[test]
    fn presses_and_keys_are_never_merged() {
        let down = DisplayEvent::PointerDown {
            x: 1,
            y: 1,
            button: 1,
        };
        assert_eq!(merged(mv(1, 1), down), None);
        assert_eq!(merged(down, down), None);
    }

    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_core::backend::{Event, NodeKind, ParentRef, WidgetId};
    use xui_core::router::WidgetHost;
    use xui_core::Rect;

    use super::super::test_support::client_backend;
    use super::super::{Node, Window};

    const W: WindowId = WindowId::from_raw(1);

    #[derive(Default)]
    struct Log(RefCell<Vec<Event>>);

    impl WidgetHost for Log {
        fn deliver(&self, _target: WidgetId, event: &Event) -> bool {
            self.0.borrow_mut().push(*event);
            true
        }
    }

    fn rig() -> (LazyOSBackend, Rc<Log>) {
        let backend = client_backend();
        let log = Rc::new(Log::default());
        backend
            .windows
            .borrow_mut()
            .insert(W.raw(), Window::for_tests(log.clone()));
        backend.nodes.borrow_mut().push((
            WidgetId::from_raw(1),
            Node {
                window: W,
                parent: ParentRef::Window(W),
                bounds: Rect::new(0, 0, 300, 300),
                visible: true,
                enabled: true,
                focus_stop: false,
                wants_tab: false,
                popup: false,
                text: String::new(),
                painter: None,
                clip: None,
                kind: NodeKind::Container,
            },
        ));
        (backend, log)
    }

    fn kinds(log: &Log) -> Vec<&'static str> {
        log.0
            .borrow()
            .iter()
            .filter_map(|event| match event {
                Event::MouseMove { .. } => Some("move"),
                Event::MouseDown { .. } => Some("down"),
                Event::MouseUp { .. } => Some("up"),
                Event::MouseWheel { .. } => Some("wheel"),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_batch_keeps_the_order_and_loses_only_the_moves_in_between() {
        let (backend, log) = rig();
        let mut held = Coalescer::default();
        let down = DisplayEvent::PointerDown {
            x: 10,
            y: 10,
            button: 0x110,
        };
        let up = DisplayEvent::PointerUp {
            x: 40,
            y: 40,
            button: 0x110,
        };
        for event in [mv(1, 1), mv(5, 5), mv(10, 10), down, mv(20, 20), mv(30, 30), mv(40, 40), up] {
            held.push(&backend, W, event);
        }
        held.flush(&backend, W);
        assert_eq!(kinds(&log), ["move", "down", "move", "up"]);
        let events = log.0.borrow();
        assert!(matches!(events[0], Event::MouseMove { x: 10, y: 10, .. }));
        assert!(matches!(events[2], Event::MouseMove { x: 40, y: 40, .. }));
    }

    #[test]
    fn a_soak_of_moves_and_rolls_delivers_the_last_move_and_every_roll() {
        let (backend, log) = rig();
        let mut held = Coalescer::default();
        for round in 0..10_000 {
            held.push(&backend, W, mv(round % 200, 7));
        }
        held.flush(&backend, W);
        for _ in 0..1_000 {
            held.push(&backend, W, wheel(5, 5, 1));
        }
        held.flush(&backend, W);
        let kinds = kinds(&log);
        assert_eq!(kinds.len(), 1 + 1_000, "one move, every roll");
        assert_eq!(kinds[0], "move");
        assert!(kinds[1..].iter().all(|k| *k == "wheel"));
        let events = log.0.borrow();
        assert!(matches!(events[0], Event::MouseMove { x: 199, y: 7, .. }));
    }
}
