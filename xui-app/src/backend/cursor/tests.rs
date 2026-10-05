use std::rc::Rc;

use xui_core::backend::{Backend, Event, ParentRef};
use xui_core::router::WidgetHost;
use xui_core::Rect;

use super::super::test_support::client_backend;
use super::super::{Node, Window};
use super::*;
use crate::client_window::ClientWindow;
use crate::display::cursor::probe;

const W: WindowId = WindowId::from_raw(1);
const SURFACE: u64 = 77;

struct Sink;

impl WidgetHost for Sink {
    fn deliver(&self, _target: WidgetId, _event: &Event) -> bool {
        true
    }
}

fn id(raw: u64) -> WidgetId {
    WidgetId::from_raw(raw)
}

fn node(parent: ParentRef, bounds: Rect) -> Node {
    Node {
        window: W,
        parent,
        bounds,
        visible: true,
        enabled: true,
        focus_stop: false,
        text: String::new(),
        painter: None,
        clip: None,
    }
}

/// A window with a panel (1) holding a link (2, top), a field (3, middle) and
/// a plain area (4, bottom); the link has a child label (5).
fn rig() -> LazyOSBackend {
    probe::reset();
    let backend = client_backend();
    let mut window = Window::for_tests(Rc::new(Sink));
    window.client = Some(ClientWindow::for_tests(SURFACE));
    backend.windows.borrow_mut().insert(W.raw(), window);
    let panel = ParentRef::Widget(id(1));
    let mut nodes = backend.nodes.borrow_mut();
    nodes.push((id(1), node(ParentRef::Window(W), Rect::new(0, 0, 64, 64))));
    nodes.push((id(2), node(panel, Rect::new(0, 0, 64, 20))));
    nodes.push((id(3), node(panel, Rect::new(0, 20, 64, 40))));
    nodes.push((id(4), node(panel, Rect::new(0, 40, 64, 64))));
    nodes.push((
        id(5),
        node(ParentRef::Widget(id(2)), Rect::new(2, 2, 30, 18)),
    ));
    drop(nodes);
    backend.set_cursor(id(2), Cursor::Hand);
    backend.set_cursor(id(3), Cursor::Text);
    backend
}

const HAND: u32 = wire::CURSOR_SHAPE_HAND;
const TEXT: u32 = wire::CURSOR_SHAPE_TEXT;
const DEFAULT: u32 = wire::CURSOR_SHAPE_DEFAULT;

#[test]
fn hovering_a_link_sends_the_hand_once() {
    let backend = rig();
    backend.pointer_move(W, 10, 5);
    assert_eq!(probe::sent(), [(SURFACE, HAND)]);
    // Moving within it, even onto its child, changes nothing.
    backend.pointer_move(W, 20, 10);
    backend.pointer_move(W, 40, 6);
    assert_eq!(probe::sent(), [(SURFACE, HAND)]);
}

#[test]
fn leaving_restores_the_arrow_and_a_field_asks_for_the_beam() {
    let backend = rig();
    backend.pointer_move(W, 10, 5);
    backend.pointer_move(W, 10, 30);
    backend.pointer_move(W, 10, 50);
    assert_eq!(
        probe::sent(),
        [(SURFACE, HAND), (SURFACE, TEXT), (SURFACE, DEFAULT)]
    );
}

#[test]
fn a_child_takes_its_nearest_ancestors_shape() {
    let backend = rig();
    // (10, 10) is inside node 5, which asked for nothing: the link's hand.
    backend.pointer_move(W, 10, 10);
    assert_eq!(probe::sent(), [(SURFACE, HAND)]);
    // Its own request wins over the ancestor's.
    backend.set_cursor(id(5), Cursor::SizeHorizontal);
    backend.pointer_move(W, 11, 10);
    assert_eq!(
        probe::sent().last(),
        Some(&(SURFACE, wire::CURSOR_SHAPE_RESIZE_H))
    );
}

#[test]
fn a_widget_that_changes_its_request_under_the_pointer_is_heard_at_once() {
    let backend = rig();
    backend.pointer_move(W, 10, 30);
    assert_eq!(probe::sent(), [(SURFACE, TEXT)]);
    // No move: the field now wants the hand (a page decided a moment later).
    backend.set_cursor(id(3), Cursor::Hand);
    assert_eq!(probe::sent(), [(SURFACE, TEXT), (SURFACE, HAND)]);
    // Asking for what is already shown sends nothing.
    backend.set_cursor(id(3), Cursor::Hand);
    assert_eq!(probe::sent().len(), 2);
}

#[test]
fn leaving_the_window_is_the_arrow() {
    let backend = rig();
    backend.pointer_move(W, 10, 5);
    backend.pointer_move(W, -20, 5);
    assert_eq!(probe::sent(), [(SURFACE, HAND), (SURFACE, DEFAULT)]);
}

#[test]
fn a_refusal_stops_further_calls() {
    let backend = rig();
    probe::FAIL.with(|fail| fail.set(Some(-errno::EACCES)));
    backend.pointer_move(W, 10, 5);
    backend.pointer_move(W, 10, 30);
    backend.pointer_move(W, 10, 50);
    assert_eq!(probe::sent(), [(SURFACE, HAND)]);
}

#[test]
fn a_transient_failure_is_retried() {
    let backend = rig();
    probe::FAIL.with(|fail| fail.set(Some(-errno::ENOENT)));
    backend.pointer_move(W, 10, 5);
    probe::FAIL.with(|fail| fail.set(None));
    backend.pointer_move(W, 11, 5);
    assert_eq!(probe::sent(), [(SURFACE, HAND), (SURFACE, HAND)]);
}

#[test]
fn busy_overrides_every_node_and_is_undone() {
    let backend = rig();
    backend.pointer_move(W, 10, 5);
    backend.set_busy(W, true);
    backend.pointer_move(W, 10, 30);
    assert_eq!(
        probe::sent(),
        [(SURFACE, HAND), (SURFACE, wire::CURSOR_SHAPE_BUSY)]
    );
    backend.set_busy(W, false);
    // The pointer is on the field: its beam comes back at once.
    assert_eq!(probe::sent().last(), Some(&(SURFACE, TEXT)));
    // A repeated call is a no-op.
    backend.set_busy(W, false);
    assert_eq!(probe::sent().len(), 3);
}

#[test]
fn a_destroyed_node_forgets_its_request() {
    let backend = rig();
    backend.destroy(id(2));
    backend.nodes.borrow_mut().push((
        id(2),
        node(ParentRef::Widget(id(1)), Rect::new(0, 0, 64, 20)),
    ));
    backend.pointer_move(W, 10, 5);
    assert_eq!(probe::sent(), [(SURFACE, DEFAULT)]);
}

#[test]
fn every_widget_cursor_maps_to_a_distinct_wire_shape() {
    let all = [
        Cursor::Default,
        Cursor::Hand,
        Cursor::Text,
        Cursor::SizeHorizontal,
        Cursor::SizeVertical,
        Cursor::SizeNwSe,
        Cursor::SizeNeSw,
    ];
    let mut shapes: Vec<u32> = all.iter().map(|c| wire_shape(*c)).collect();
    shapes.sort_unstable();
    shapes.dedup();
    assert_eq!(shapes.len(), all.len());
    // Every one is a shape the compositor can draw.
    assert!(shapes
        .iter()
        .all(|s| cursorgfx::Shape::from_wire(*s).is_some()));
}
