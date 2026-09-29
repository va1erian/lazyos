//! Keyboard focus: delivery of focus changes, focus-stop membership, and the
//! Tab/PageUp/PageDown focus cycle.

use xui_core::backend::{Event, WidgetId, WindowId};

use super::LazyOSBackend;

impl LazyOSBackend {
    /// Give `id` the keyboard focus, notifying the widget that lost it.
    pub(super) fn set_focus(&self, id: WidgetId) {
        let old = self.focused.replace(Some(id));
        if old == Some(id) {
            return;
        }
        let Some(window) = self.window_of(id) else {
            return;
        };
        if let Some(old) = old {
            if self.window_of(old) == Some(window) {
                self.deliver(window, old, &Event::KillFocus);
            }
        }
        self.deliver(window, id, &Event::SetFocus);
    }

    /// Whether `id` is a focus stop (a Tab-order control or a button-like one).
    pub(super) fn is_focus_stop(&self, id: WidgetId) -> bool {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .is_some_and(|(_, node)| node.visible && node.enabled && node.focus_stop)
    }

    /// Move the keyboard focus to the next (or previous) focus stop.
    pub(super) fn cycle_focus(&self, window: WindowId, forward: bool) {
        let stops: Vec<WidgetId> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| {
                node.window == window && node.visible && node.enabled && node.focus_stop
            })
            .map(|(id, _)| *id)
            .collect();
        if stops.is_empty() {
            return;
        }
        let current = self
            .focused
            .get()
            .and_then(|id| stops.iter().position(|stop| *stop == id));
        let next = match current {
            Some(index) if forward => (index + 1) % stops.len(),
            Some(index) => (index + stops.len() - 1) % stops.len(),
            None if forward => 0,
            None => stops.len() - 1,
        };
        self.set_focus(stops[next]);
    }
}
