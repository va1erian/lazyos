//! Node-table helpers: widget-id allocation, lookup, and event delivery.

use std::cell::Cell;

use xui_core::backend::{Event, WidgetId, WindowId};

use super::{LazyOSBackend, Node};

impl LazyOSBackend {
    pub(super) fn allocate(cell: &Cell<u64>) -> u64 {
        let id = cell.get();
        cell.set(id + 1);
        id
    }

    /// The window a node belongs to.
    pub(super) fn window_of(&self, id: WidgetId) -> Option<WindowId> {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| node.window)
    }

    /// Offer `event` to the sink installed by `run_app`.
    pub(super) fn deliver(&self, window: WindowId, target: WidgetId, event: &Event) -> bool {
        let sink = self
            .windows
            .borrow()
            .get(&window.raw())
            .and_then(|entry| entry.sink.clone());
        sink.is_some_and(|sink| sink.deliver(target, event))
    }

    pub(super) fn with_node<R>(&self, id: WidgetId, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        self.nodes
            .borrow_mut()
            .iter_mut()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| f(node))
    }
}
