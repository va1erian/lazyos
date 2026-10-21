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
//!
//! Only a stock widget holding the capture gets this (a scroll bar's thumb, a
//! splitter: they work from the last position). A self-painted node
//! (`NodeKind::Custom`) that captured the pointer sees every move: Paint's
//! brush and pencil join the points of a stroke, a selection or a terminal
//! mouse report counts them, and a chord instead of the path would show. With
//! no capture the pointer is only hovering and every move is delivered.

use xui_core::backend::{NodeKind, WindowId};

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

impl LazyOSBackend {
    /// Whether a pointer move may be held back for a later one: only while a
    /// stock (not self-painted) node holds the capture.
    fn coalesces_moves(&self) -> bool {
        let Some(captured) = self.captured.get() else {
            return false;
        };
        self.nodes
            .borrow()
            .iter()
            .find(|(id, _)| *id == captured)
            .is_some_and(|(_, node)| node.kind != NodeKind::Custom)
    }
}

impl Coalescer {
    /// Route `event`, holding it back when it is a move that may be merged.
    pub(super) fn push(&mut self, backend: &LazyOSBackend, window: WindowId, event: DisplayEvent) {
        let holdable =
            matches!(event, DisplayEvent::PointerMove { .. }) && backend.coalesces_moves();
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

    use xui_core::backend::{Backend, Event, NodeKind, ParentRef, WidgetId};
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

    /// A rig whose one node holds the capture, as a `kind` node.
    fn captured_rig(kind: NodeKind) -> (LazyOSBackend, Rc<Log>) {
        let (backend, log) = rig();
        backend.nodes.borrow_mut()[0].1.kind = kind;
        backend.set_capture(WidgetId::from_raw(1));
        (backend, log)
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
        // A scroll bar's thumb drag: a stock node holds the capture.
        let (backend, log) = captured_rig(NodeKind::Container);
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
        let (backend, log) = captured_rig(NodeKind::Container);
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

    #[test]
    fn a_self_painted_node_that_holds_the_capture_sees_every_move() {
        // Paint's brush: a stroke is the path of the moves, not their chord.
        let (backend, log) = captured_rig(NodeKind::Custom);
        let mut held = Coalescer::default();
        for step in 0..50 {
            held.push(&backend, W, mv(step, step * 2));
        }
        held.flush(&backend, W);
        let events = log.0.borrow();
        assert_eq!(events.len(), 50);
        for (step, event) in events.iter().enumerate() {
            let step = step as i32;
            assert!(matches!(event, Event::MouseMove { x, y, .. } if *x == step && *y == step * 2));
        }
    }

    #[test]
    fn without_a_capture_every_move_is_delivered() {
        let (backend, log) = rig();
        let mut held = Coalescer::default();
        for step in 0..20 {
            held.push(&backend, W, mv(step, 3));
        }
        held.flush(&backend, W);
        assert_eq!(kinds(&log).len(), 20);
    }

    #[test]
    fn a_capture_ending_while_a_move_is_held_still_delivers_the_last_one() {
        let (backend, log) = captured_rig(NodeKind::Container);
        let mut held = Coalescer::default();
        held.push(&backend, W, mv(1, 1));
        held.push(&backend, W, mv(2, 2));
        backend.release_capture();
        // The held move merges with the next one; nothing is lost or reordered.
        held.push(&backend, W, mv(3, 3));
        held.flush(&backend, W);
        let moves: Vec<(i32, i32)> = log
            .0
            .borrow()
            .iter()
            .filter_map(|e| match e {
                Event::MouseMove { x, y, .. } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(moves, [(3, 3)]);
    }
}
