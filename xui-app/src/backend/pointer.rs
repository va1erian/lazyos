//! Pointer routing: hit testing, capture, double-click synthesis and the
//! translation of window pixels into the target widget's local coordinates.

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::{Modifiers, MouseButton, Rect};

use crate::sys;

use super::double_click::PressKind;
use super::geometry::{absolute_bounds, hit, translate};
use super::LazyOSBackend;

/// The `MouseWheel` delta of one notch: Windows' `WHEEL_DELTA`, the unit xui's
/// widgets and `xui-litehtml` (which divides by it) expect.
const WHEEL_NOTCH: i32 = 120;

impl LazyOSBackend {
    /// The node a pointer event at window point `(x, y)` goes to, with its
    /// window-absolute bounds: the capturing node in `window` if any (so a drag
    /// survives leaving it), else the topmost node under the point.
    fn pointer_target(&self, window: WindowId, x: i32, y: i32) -> Option<(WidgetId, Rect)> {
        let nodes = self.nodes.borrow();
        if let Some(id) = self.captured.get() {
            let in_window = nodes
                .iter()
                .any(|(n, node)| *n == id && node.window == window);
            if in_window {
                if let Some(abs) = absolute_bounds(&nodes, id) {
                    return Some((id, abs));
                }
            }
        }
        hit(&nodes, window, x, y)
    }

    /// Deliver a pointer `event` (window coordinates) to `target`, translated to
    /// its local coordinates; with no target it goes to the window unchanged.
    fn deliver_pointer(&self, window: WindowId, target: Option<(WidgetId, Rect)>, event: Event) {
        match target {
            Some((id, abs)) => {
                let (x, y) = self.pointer.get();
                self.deliver(window, id, &translate(event, x - abs.left, y - abs.top));
            }
            None => {
                self.deliver(window, WidgetId::NONE, &event);
            }
        }
    }

    /// Route one pointer move (window-relative coordinates).
    pub(super) fn pointer_move(&self, window: WindowId, x: i32, y: i32) {
        self.pointer.set((x, y));
        let target = self.pointer_target(window, x, y);
        self.track_leave(window, x, y, target.map(|(id, _)| id));
        let event = Event::MouseMove {
            x,
            y,
            modifiers: Modifiers::NONE,
        };
        self.deliver_pointer(window, target, event);
    }

    /// Tell the hovered node it was left: when the pointer moves onto another
    /// node of the window (as `xui-canvas`'s `set_hover` does, so a tooltip or
    /// hover highlight clears), and when a move lands outside the window with
    /// no capture: the compositor sends a panel exactly one such move when the
    /// pointer leaves it.
    fn track_leave(&self, window: WindowId, x: i32, y: i32, target: Option<WidgetId>) {
        let size = self
            .windows
            .borrow()
            .get(&window.raw())
            .map(|entry| (entry.width, entry.height));
        let outside = size.is_some_and(|(w, h)| x < 0 || y < 0 || x >= w || y >= h);
        let next = target.map(|id| (window, id));
        match self.hovered.get() {
            // Only the window the pointer left; another window's hover stays.
            Some((owner, _)) if owner != window && outside && target.is_none() => {}
            Some((owner, left)) if owner == window && Some((owner, left)) != next => {
                self.hovered.set(next);
                self.deliver(window, left, &Event::MouseLeave);
            }
            _ => self.hovered.set(next),
        }
    }

    /// Route a wheel roll of `notches` (positive scrolls up) at window point
    /// `(x, y)` to the node under the pointer.
    pub(super) fn pointer_wheel(&self, window: WindowId, x: i32, y: i32, notches: i32) {
        self.pointer.set((x, y));
        let delta = notches
            .saturating_mul(WHEEL_NOTCH)
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        if delta == 0 {
            return;
        }
        let target = self.pointer_target(window, x, y);
        let event = Event::MouseWheel {
            delta,
            horizontal: false,
            x,
            y,
            modifiers: Modifiers::NONE,
        };
        self.deliver_pointer(window, target, event);
    }

    /// Route one pointer press: click-to-focus, then the press itself, or a
    /// double-click in place of a quick second press on the same widget.
    pub(super) fn pointer_down(&self, window: WindowId, x: i32, y: i32, button: MouseButton) {
        self.pointer_down_at(window, x, y, button, sys::clock_ticks());
    }

    /// [`pointer_down`](Self::pointer_down) at an explicit PIT tick `now`.
    pub(super) fn pointer_down_at(
        &self,
        window: WindowId,
        x: i32,
        y: i32,
        button: MouseButton,
        now: u64,
    ) {
        self.pointer.set((x, y));
        let under = hit(&self.nodes.borrow(), window, x, y).map(|(id, _)| id);
        if let Some(id) = under {
            if self.is_focus_stop(id) {
                self.set_focus(id);
            }
        }
        let target = self.pointer_target(window, x, y);
        let id = target.map_or(WidgetId::NONE, |(id, _)| id);
        // A press on nothing never pairs. The tracker borrow ends here,
        // before any widget code runs.
        let kind = if id == WidgetId::NONE {
            PressKind::Down
        } else {
            self.clicks
                .borrow_mut()
                .press(now, window, x, y, button, id)
        };
        self.deliver_pointer(window, target, kind.event(x, y, button, Modifiers::NONE));
    }

    /// Route one pointer release.
    pub(super) fn pointer_up(&self, window: WindowId, x: i32, y: i32, button: MouseButton) {
        self.pointer.set((x, y));
        let target = self.pointer_target(window, x, y);
        let event = Event::MouseUp {
            x,
            y,
            button,
            modifiers: Modifiers::NONE,
        };
        self.deliver_pointer(window, target, event);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_core::backend::{Backend, ParentRef};
    use xui_core::router::WidgetHost;

    use super::super::test_support::client_backend;
    use super::super::{Node, Window};
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

    /// A backend with a window, a panel, a canvas 27 px down (like a toolbar
    /// above it) and a sibling to its right, plus the event log.
    fn rig() -> (LazyOSBackend, Rc<Log>) {
        let backend = client_backend();
        let log = Rc::new(Log::default());
        backend
            .windows
            .borrow_mut()
            .insert(W.raw(), Window::for_tests(log.clone()));
        let panel = ParentRef::Widget(id(1));
        let mut nodes = backend.nodes.borrow_mut();
        nodes.push((id(1), node(ParentRef::Window(W), Rect::new(0, 0, 300, 300))));
        nodes.push((id(2), node(panel, Rect::new(0, 27, 100, 127))));
        nodes.push((id(3), node(panel, Rect::new(150, 27, 250, 127))));
        drop(nodes);
        (backend, log)
    }

    fn last(log: &Log) -> (WidgetId, Event) {
        log.0.borrow().last().cloned().expect("an event")
    }

    #[test]
    fn events_arrive_in_node_local_coordinates() {
        let (backend, log) = rig();
        backend.pointer_move(W, 10, 40);
        let (target, event) = last(&log);
        assert_eq!(target, id(2));
        assert!(matches!(event, Event::MouseMove { x: 10, y: 13, .. }));
    }

    #[test]
    fn two_quick_presses_on_one_widget_are_a_double_click() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 10, 40, MouseButton::Left, 100);
        assert!(matches!(
            last(&log).1,
            Event::MouseDown { x: 10, y: 13, .. }
        ));
        backend.pointer_down_at(W, 11, 41, MouseButton::Left, 120);
        let (target, event) = last(&log);
        assert_eq!(target, id(2));
        assert!(matches!(
            event,
            Event::MouseDoubleClick { x: 11, y: 14, .. }
        ));
        // A third quick press starts over.
        backend.pointer_down_at(W, 11, 41, MouseButton::Left, 125);
        assert!(matches!(last(&log).1, Event::MouseDown { .. }));
    }

    #[test]
    fn presses_on_different_widgets_or_too_slow_stay_plain() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 10, 40, MouseButton::Left, 100);
        backend.pointer_down_at(W, 160, 40, MouseButton::Left, 101);
        assert!(matches!(last(&log).1, Event::MouseDown { .. }));
        backend.pointer_down_at(W, 160, 40, MouseButton::Left, 500);
        assert!(matches!(last(&log).1, Event::MouseDown { .. }));
    }

    #[test]
    fn a_destroyed_target_drops_the_pending_click() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 10, 40, MouseButton::Left, 100);
        backend.destroy(id(2));
        backend.pointer_down_at(W, 10, 40, MouseButton::Left, 101);
        // The node is gone: the panel gets a plain press, not a double-click.
        assert!(matches!(last(&log).1, Event::MouseDown { .. }));
        assert_eq!(last(&log).0, id(1));
    }

    #[test]
    fn capture_routes_moves_and_releases_outside_the_node() {
        let (backend, log) = rig();
        backend.set_capture(id(2));
        backend.pointer_move(W, 250, 200);
        let (target, event) = last(&log);
        assert_eq!(target, id(2));
        assert!(matches!(event, Event::MouseMove { x: 250, y: 173, .. }));
        backend.pointer_up(W, 250, 200, MouseButton::Left);
        assert_eq!(last(&log).0, id(2));
        backend.release_capture();
        assert!(matches!(last(&log).1, Event::CaptureChanged));
        backend.pointer_move(W, 250, 200);
        assert_eq!(last(&log).0, id(1));
    }

    #[test]
    fn destroying_the_captured_node_ends_the_capture() {
        let (backend, log) = rig();
        backend.set_capture(id(2));
        backend.destroy(id(2));
        backend.pointer_move(W, 10, 40);
        assert_eq!(last(&log).0, id(1));
    }

    #[test]
    fn a_wheel_notch_reaches_the_widget_under_the_pointer_in_local_coordinates() {
        let (backend, log) = rig();
        backend.pointer_wheel(W, 10, 40, 1);
        let (target, event) = last(&log);
        assert_eq!(target, id(2));
        assert!(matches!(
            event,
            Event::MouseWheel {
                delta: 120,
                horizontal: false,
                x: 10,
                y: 13,
                ..
            }
        ));
        backend.pointer_wheel(W, 160, 40, -2);
        let (target, event) = last(&log);
        assert_eq!(target, id(3));
        assert!(matches!(event, Event::MouseWheel { delta: -240, .. }));
    }

    #[test]
    fn a_zero_wheel_is_dropped_and_a_huge_one_saturates() {
        let (backend, log) = rig();
        backend.pointer_wheel(W, 10, 40, 0);
        assert!(log.0.borrow().is_empty(), "no event for zero notches");
        backend.pointer_wheel(W, 10, 40, i32::MAX);
        assert!(matches!(
            last(&log).1,
            Event::MouseWheel {
                delta: i16::MAX,
                ..
            }
        ));
        backend.pointer_wheel(W, 10, 40, i32::MIN);
        assert!(matches!(
            last(&log).1,
            Event::MouseWheel {
                delta: i16::MIN,
                ..
            }
        ));
    }

    #[test]
    fn a_wheel_soak_delivers_every_notch_in_order() {
        let (backend, log) = rig();
        for round in 0..10_000i32 {
            backend.pointer_wheel(W, 10, 40, if round % 2 == 0 { 1 } else { -1 });
        }
        let log = log.0.borrow();
        assert_eq!(log.len(), 10_000);
        for (index, (target, event)) in log.iter().enumerate() {
            let want = if index % 2 == 0 { 120 } else { -120 };
            assert_eq!(*target, id(2));
            assert!(matches!(event, Event::MouseWheel { delta, .. } if *delta == want));
        }
    }

    #[test]
    fn leaving_the_window_tells_the_hovered_node_once() {
        let (backend, log) = rig();
        backend.pointer_move(W, 10, 40);
        let hovered = last(&log).0;
        // The compositor's last move for a panel the pointer left lands
        // outside the window and on no node.
        let before = log.0.borrow().len();
        backend.pointer_move(W, -5, 40);
        assert!(log.0.borrow()[before..].contains(&(hovered, Event::MouseLeave)));
        let count = log.0.borrow().len();
        backend.pointer_move(W, -6, 40);
        let leaves = log.0.borrow()[count..]
            .iter()
            .filter(|(_, event)| *event == Event::MouseLeave)
            .count();
        assert_eq!(leaves, 0, "told once");
    }

    #[test]
    fn moving_onto_another_node_tells_the_old_one_it_was_left() {
        let (backend, log) = rig();
        backend.pointer_move(W, 10, 40);
        let first = last(&log).0;
        backend.pointer_move(W, 160, 40);
        let second = last(&log).0;
        assert_ne!(first, second);
        let events = log.0.borrow();
        assert!(
            events.contains(&(first, Event::MouseLeave)),
            "the old node hears it was left"
        );
        assert!(!events.contains(&(second, Event::MouseLeave)));
        let leave = events
            .iter()
            .position(|e| *e == (first, Event::MouseLeave))
            .unwrap();
        let moved = events
            .iter()
            .rposition(|(id, e)| *id == second && matches!(e, Event::MouseMove { .. }))
            .unwrap();
        assert!(leave < moved, "the leave comes before the new node's move");
    }

    #[test]
    fn moving_within_one_node_sends_no_leave() {
        let (backend, log) = rig();
        backend.pointer_move(W, 10, 40);
        backend.pointer_move(W, 12, 41);
        assert!(!log.0.borrow().iter().any(|(_, e)| *e == Event::MouseLeave));
    }

    #[test]
    fn another_windows_outside_move_leaves_this_hover_alone() {
        let (backend, log) = rig();
        backend.pointer_move(W, 10, 40);
        backend.pointer_move(WindowId::from_raw(2), -5, 40);
        assert!(!log.0.borrow().iter().any(|(_, e)| *e == Event::MouseLeave));
    }
}
