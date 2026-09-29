//! Unit tests for the offscreen backend's pointer capture.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::Rect;
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, ParentRef, PlatformSpec};
use xui_core::message::{Modifiers, MouseButton};

use super::OffscreenBackend;
use super::tests::Recorder;

#[test]
fn a_captured_node_receives_an_out_of_bounds_move() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("capture")).unwrap();
    let node = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(10, 10, 60, 50)),
        )
        .unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));

    backend.set_capture(node);
    let consumed = backend.inject(
        window,
        Event::MouseMove {
            x: 500,
            y: 500,
            modifiers: Modifiers::NONE,
        },
    );

    assert!(consumed, "the captured node consumes the move");
    let events = log.borrow();
    let (target, event) = events.last().expect("an event was delivered");
    assert_eq!(*target, node, "the captured node is the target");
    // Far outside the node: the local point stays negative.
    assert!(matches!(
        event,
        Event::MouseMove { x, y, .. } if *x == 490 && *y == 490
    ));
}

#[test]
fn releasing_capture_tells_the_captured_node() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("capture")).unwrap();
    let node = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(10, 10, 60, 50)),
        )
        .unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));

    backend.set_capture(node);
    backend.release_capture();

    let events = log.borrow();
    assert!(matches!(
        events.as_slice(),
        [(target, Event::CaptureChanged)] if *target == node
    ));
    // A move after release no longer reaches the node.
    let before = events.len();
    backend.inject(
        window,
        Event::MouseMove {
            x: 500,
            y: 500,
            modifiers: Modifiers::NONE,
        },
    );
    assert_eq!(log.borrow().len(), before, "capture is gone");
}

#[test]
fn a_mouse_up_reaches_the_captured_node_outside_it() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("capture")).unwrap();
    let node = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 40, 40)),
        )
        .unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));

    backend.set_capture(node);
    backend.inject(
        window,
        Event::MouseUp {
            x: 200,
            y: 200,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    );

    assert!(matches!(
        log.borrow().last(),
        Some((target, Event::MouseUp { .. })) if *target == node
    ));
}
