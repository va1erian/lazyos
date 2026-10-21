//! Routing the wheel: to the widget under the pointer, or, when that widget
//! cannot scroll, to the scroll view that holds it.
//!
//! `xui-core` has no event bubbling (`WidgetHost::deliver` says so), and a
//! scroll view's content widgets are its children: a label, a wrapped text, a
//! link or a button under the pointer is the deepest node, nothing there
//! reads the wheel, and the roll was lost unless the pointer happened to be on
//! bare view. So a roll over a widget that does not take the wheel itself goes
//! to the nearest `ScrollView` above it. A widget that takes the wheel (a
//! list, a tree, an editor, a custom painted view, a combo box) keeps it, even
//! inside a scroll view.

use xui_core::backend::{Event, NodeKind, ParentRef, WidgetId, WindowId};
use xui_core::Rect;

use super::geometry::{absolute_bounds, node_of};
use super::{LazyOSBackend, Node};

/// The `MouseWheel` delta of one notch: Windows' `WHEEL_DELTA`, the unit xui's
/// widgets and `xui-litehtml` (which divides by it) expect.
const WHEEL_NOTCH: i16 = 120;
/// The most notches one roll is cut into: the compositor sends a quick roll as one
/// event carrying its notch count, and a big count must not stall the loop.
const MAX_NOTCHES: u32 = 16;

/// Whether a roll over a node of this kind is lost unless a scroll view above
/// takes it: the kinds that show or frame something and read no wheel.
fn passes_wheel(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Container
            | NodeKind::Label
            | NodeKind::Button
            | NodeKind::CheckBox
            | NodeKind::Radio
            | NodeKind::GroupBox
            | NodeKind::Separator
            | NodeKind::ProgressBar
            | NodeKind::Toolbar
            | NodeKind::Tabs
            | NodeKind::StatusBar
    )
}

/// The node a roll over `hit` goes to: the nearest scroll view above it when
/// `hit` and every node between pass the wheel, else `hit` itself.
pub(super) fn wheel_target(nodes: &[(WidgetId, Node)], hit: WidgetId) -> WidgetId {
    let mut current = hit;
    // A chain is shorter than the table; the bound stops a cyclic one.
    for _ in 0..=nodes.len() {
        let Some(node) = node_of(nodes, current) else {
            return hit;
        };
        if node.kind == NodeKind::ScrollView {
            return current;
        }
        if !passes_wheel(node.kind) {
            return hit;
        }
        match node.parent {
            ParentRef::Widget(parent) => current = parent,
            ParentRef::Window(_) => return hit,
        }
    }
    hit
}

impl LazyOSBackend {
    /// The node a wheel roll at `(x, y)` goes to, with its window-absolute
    /// bounds: the capturing node if any, else [`wheel_target`] of the
    /// topmost node under the point.
    fn wheel_receiver(&self, window: WindowId, x: i32, y: i32) -> Option<(WidgetId, Rect)> {
        let under = self.pointer_target(window, x, y)?;
        if self.captured.get().is_some() {
            return Some(under);
        }
        let nodes = self.nodes.borrow();
        let id = wheel_target(&nodes, under.0);
        if id == under.0 {
            return Some(under);
        }
        absolute_bounds(&nodes, id).map(|abs| (id, abs))
    }

    /// Route a wheel roll of `notches` (positive scrolls up) at window point
    /// `(x, y)`, as one `MouseWheel` of one notch per notch (at most
    /// [`MAX_NOTCHES`]): `xui`'s scroll view and its relatives move one step per
    /// event whatever its delta, so a roll of three notches delivered as one
    /// event scrolled one step, and one delivered as three scrolls three, as a
    /// mouse that sends a notch at a time does.
    pub(super) fn pointer_wheel(&self, window: WindowId, x: i32, y: i32, notches: i32) {
        self.pointer.set((x, y));
        let count = notches.unsigned_abs().min(MAX_NOTCHES);
        let delta = if notches < 0 {
            -WHEEL_NOTCH
        } else {
            WHEEL_NOTCH
        };
        for _ in 0..count {
            // Looked up for each notch: a step may move the page (or end the capture).
            let target = self.wheel_receiver(window, x, y);
            let event = Event::MouseWheel {
                delta,
                horizontal: false,
                x,
                y,
                modifiers: self.modifiers.get(),
            };
            self.deliver_pointer(window, target, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_core::backend::Backend;
    use xui_core::router::WidgetHost;

    use super::super::test_support::client_backend;
    use super::super::Window;
    use super::*;

    const W: WindowId = WindowId::from_raw(1);

    #[derive(Default)]
    struct Log(RefCell<Vec<(WidgetId, Event)>>);

    impl WidgetHost for Log {
        fn deliver(&self, target: WidgetId, event: &Event) -> bool {
            self.0.borrow_mut().push((target, *event));
            true
        }
    }

    fn id(raw: u64) -> WidgetId {
        WidgetId::from_raw(raw)
    }

    fn node(parent: ParentRef, bounds: Rect, kind: NodeKind) -> Node {
        Node {
            window: W,
            parent,
            bounds,
            visible: true,
            enabled: true,
            focus_stop: false,
            wants_tab: false,
            popup: false,
            text: String::new(),
            painter: None,
            clip: None,
            kind,
        }
    }

    /// Window 1: a panel (1) holding a scroll view (2) at (10, 20) that holds a
    /// label (3), a button (4), a list (5) with its bar (6) and a custom
    /// widget (7); beside it a lone label (8) and a lone list (9).
    fn rig() -> (LazyOSBackend, Rc<Log>) {
        let backend = client_backend();
        let log = Rc::new(Log::default());
        backend
            .windows
            .borrow_mut()
            .insert(W.raw(), Window::for_tests(log.clone()));
        let panel = ParentRef::Widget(id(1));
        let view = ParentRef::Widget(id(2));
        let list = ParentRef::Widget(id(5));
        let mut nodes = backend.nodes.borrow_mut();
        let mut add =
            |n: u64, parent, rect: Rect, kind| nodes.push((id(n), node(parent, rect, kind)));
        add(
            1,
            ParentRef::Window(W),
            Rect::new(0, 0, 400, 300),
            NodeKind::Container,
        );
        add(2, panel, Rect::new(10, 20, 210, 220), NodeKind::ScrollView);
        add(3, view, Rect::new(0, 0, 150, 30), NodeKind::Label);
        add(4, view, Rect::new(0, 40, 80, 70), NodeKind::Button);
        add(5, view, Rect::new(0, 80, 150, 140), NodeKind::ListView);
        add(6, list, Rect::new(138, 0, 150, 60), NodeKind::Container);
        add(7, view, Rect::new(0, 150, 150, 190), NodeKind::Custom);
        add(8, panel, Rect::new(250, 20, 350, 50), NodeKind::Label);
        add(9, panel, Rect::new(250, 60, 350, 120), NodeKind::ListView);
        drop(nodes);
        (backend, log)
    }

    fn roll(backend: &LazyOSBackend, log: &Log, x: i32, y: i32) -> (WidgetId, Event) {
        backend.pointer_wheel(W, x, y, 1);
        let last = log.0.borrow().last().cloned().expect("an event");
        last
    }

    #[test]
    fn a_roll_of_several_notches_is_one_event_per_notch() {
        let (backend, log) = rig();
        backend.pointer_wheel(W, 30, 30, -3);
        let events = log.0.borrow();
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|(target, event)| *target == id(2)
            && matches!(event, Event::MouseWheel { delta: -120, .. })));
        drop(events);
        backend.pointer_wheel(W, 30, 30, i32::MAX);
        assert_eq!(
            log.0.borrow().len(),
            3 + MAX_NOTCHES as usize,
            "a huge roll is cut"
        );
        backend.pointer_wheel(W, 30, 30, 0);
        assert_eq!(
            log.0.borrow().len(),
            3 + MAX_NOTCHES as usize,
            "no event for zero notches"
        );
    }

    #[test]
    fn a_roll_over_a_label_or_a_button_goes_to_the_scroll_view_in_its_coordinates() {
        let (backend, log) = rig();
        // The label is at window (10, 20): the point (30, 30) is local (20, 10) in the view.
        let (target, event) = roll(&backend, &log, 30, 30);
        assert_eq!(target, id(2), "the label passes the wheel up");
        assert!(matches!(
            event,
            Event::MouseWheel {
                delta: 120,
                x: 20,
                y: 10,
                ..
            }
        ));
        // The button is at window (10, 60)..(90, 90).
        assert_eq!(roll(&backend, &log, 20, 70).0, id(2), "so does a button");
    }

    #[test]
    fn a_roll_over_the_bare_view_stays_with_the_view() {
        let (backend, log) = rig();
        assert_eq!(roll(&backend, &log, 200, 210).0, id(2));
    }

    #[test]
    fn a_widget_that_reads_the_wheel_keeps_it_inside_a_scroll_view() {
        let (backend, log) = rig();
        // The list is at window (10, 100)..(160, 160), its bar at (148, 100)..(160, 160),
        // the custom widget at (10, 170)..(160, 210).
        assert_eq!(roll(&backend, &log, 50, 120).0, id(5));
        assert_eq!(
            roll(&backend, &log, 154, 120).0,
            id(6),
            "a list's own bar is the list's business"
        );
        assert_eq!(roll(&backend, &log, 50, 180).0, id(7));
    }

    #[test]
    fn outside_any_scroll_view_nothing_is_rerouted() {
        let (backend, log) = rig();
        assert_eq!(
            roll(&backend, &log, 260, 30).0,
            id(8),
            "a label with no view above it"
        );
        assert_eq!(roll(&backend, &log, 260, 70).0, id(9));
    }

    #[test]
    fn a_capture_keeps_the_roll() {
        let (backend, log) = rig();
        backend.set_capture(id(3));
        assert_eq!(
            roll(&backend, &log, 30, 30).0,
            id(3),
            "a drag in progress owns the pointer"
        );
        backend.release_capture();
        assert_eq!(roll(&backend, &log, 30, 30).0, id(2));
    }

    #[test]
    fn the_target_walk_stops_on_a_cyclic_chain() {
        let (backend, _log) = rig();
        let mut nodes = backend.nodes.borrow_mut();
        // 3 -> 4 -> 3: a corrupted table must not spin.
        nodes
            .iter_mut()
            .find(|(n, _)| *n == id(3))
            .unwrap()
            .1
            .parent = ParentRef::Widget(id(4));
        nodes
            .iter_mut()
            .find(|(n, _)| *n == id(4))
            .unwrap()
            .1
            .parent = ParentRef::Widget(id(3));
        assert_eq!(wheel_target(&nodes, id(3)), id(3));
    }

    #[test]
    fn a_wheel_soak_over_a_label_reaches_the_view_in_order() {
        let (backend, log) = rig();
        for round in 0..10_000i32 {
            backend.pointer_wheel(W, 30, 30, if round % 2 == 0 { 1 } else { -1 });
        }
        let log = log.0.borrow();
        assert_eq!(log.len(), 10_000);
        for (index, (target, event)) in log.iter().enumerate() {
            let want = if index % 2 == 0 { 120 } else { -120 };
            assert_eq!(*target, id(2));
            assert!(matches!(event, Event::MouseWheel { delta, .. } if *delta == want));
        }
    }
}
