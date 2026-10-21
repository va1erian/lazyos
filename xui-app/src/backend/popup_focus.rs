//! A press outside an open popup (a menu level) takes the keyboard focus from
//! it, so the menu closes; `focus_holder` tells a widget where the focus went.

use xui_core::backend::{Event, WidgetId, WindowId};

use super::LazyOSBackend;

impl LazyOSBackend {
    /// The node that holds the keyboard focus in `window`, if any.
    pub(super) fn focus_holder(&self, window: WindowId) -> Option<WidgetId> {
        self.focused
            .get()
            .filter(|id| self.window_of(*id) == Some(window))
    }

    /// Whether `id` is a transient popup surface.
    pub(super) fn is_popup(&self, id: WidgetId) -> bool {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .is_some_and(|(_, node)| node.popup)
    }

    /// Takes the keyboard focus from a popup surface of `window` that holds it,
    /// who is told `KillFocus`: a press outside the popup dismisses it (an open
    /// menu closes). Any other holder keeps the focus.
    pub(super) fn dismiss_popup_focus(&self, window: WindowId) {
        let Some(popup) = self
            .focus_holder(window)
            .filter(|popup| self.is_popup(*popup))
        else {
            return;
        };
        self.focused.set(None);
        self.deliver(window, popup, &Event::KillFocus);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_core::backend::ParentRef;
    use xui_core::router::WidgetHost;
    use xui_core::{MouseButton, Rect};

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

    fn node(parent: ParentRef, bounds: Rect, focus_stop: bool, popup: bool) -> Node {
        Node {
            window: W,
            parent,
            bounds,
            visible: true,
            enabled: true,
            focus_stop,
            wants_tab: false,
            popup,
            text: String::new(),
            painter: None,
            clip: None,
            kind: xui_core::backend::NodeKind::Container,
        }
    }

    /// A window with a panel (1), a plain widget (2), a focus stop (3) and a
    /// popup (4) that holds the focus, plus the event log.
    fn rig() -> (LazyOSBackend, Rc<Log>) {
        let backend = client_backend();
        let log = Rc::new(Log::default());
        backend
            .windows
            .borrow_mut()
            .insert(W.raw(), Window::for_tests(log.clone()));
        let panel = ParentRef::Widget(id(1));
        {
            let mut nodes = backend.nodes.borrow_mut();
            let all = Rect::new(0, 0, 300, 300);
            nodes.push((id(1), node(ParentRef::Window(W), all, false, false)));
            let (plain, stop, popup) = (
                Rect::new(0, 27, 100, 127),
                Rect::new(150, 27, 250, 127),
                Rect::new(0, 200, 100, 280),
            );
            nodes.push((id(2), node(panel, plain, false, false)));
            nodes.push((id(3), node(panel, stop, true, false)));
            nodes.push((id(4), node(panel, popup, false, true)));
        }
        backend.set_focus(id(4));
        log.0.borrow_mut().clear();
        (backend, log)
    }

    fn killed(log: &Log, target: WidgetId) -> bool {
        log.0.borrow().contains(&(target, Event::KillFocus))
    }

    #[test]
    fn a_press_on_a_plain_widget_takes_the_focus_from_an_open_popup() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 10, 40, MouseButton::Left, 100);
        assert!(killed(&log, id(4)), "the popup is dismissed");
        assert_eq!(backend.focus_holder(W), None);
        let (target, event) = log.0.borrow().last().cloned().expect("an event");
        assert_eq!(target, id(2), "the press still reaches the widget");
        assert!(matches!(event, Event::MouseDown { .. }));
    }

    #[test]
    fn a_press_on_bare_window_takes_the_focus_from_an_open_popup() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 290, 290, MouseButton::Left, 100);
        assert!(killed(&log, id(4)));
        assert_eq!(backend.focus_holder(W), None);
    }

    #[test]
    fn a_press_inside_a_popup_leaves_it_open() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 10, 230, MouseButton::Left, 100);
        assert!(!killed(&log, id(4)));
        assert_eq!(backend.focus_holder(W), Some(id(4)));
    }

    #[test]
    fn a_press_on_a_focus_stop_moves_the_focus_to_it() {
        let (backend, log) = rig();
        backend.pointer_down_at(W, 160, 40, MouseButton::Left, 100);
        assert!(killed(&log, id(4)), "the popup hears it lost the focus");
        assert_eq!(
            backend.focus_holder(W),
            Some(id(3)),
            "the new holder is the focus stop, which a menu bar counts as its own"
        );
    }

    #[test]
    fn a_press_elsewhere_keeps_an_ordinary_focus_holder() {
        let (backend, log) = rig();
        backend.set_focus(id(3));
        log.0.borrow_mut().clear();
        backend.pointer_down_at(W, 10, 40, MouseButton::Left, 100);
        assert_eq!(backend.focus_holder(W), Some(id(3)));
        assert!(!killed(&log, id(3)));
    }
}
