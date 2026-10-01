//! Node-table helpers: widget-id allocation, lookup, and event delivery.

use std::cell::Cell;
use std::sync::atomic::Ordering;

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::Rect;

use super::geometry::absolute_bounds;
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

    /// The window and window-absolute bounds of `id`: what a repaint of it
    /// damages (node bounds are parent-relative).
    pub(super) fn absolute_damage(&self, id: WidgetId) -> Option<(WindowId, Rect)> {
        let nodes = self.nodes.borrow();
        let window = nodes.iter().find(|(node_id, _)| *node_id == id)?.1.window;
        Some((window, absolute_bounds(&nodes, id)?))
    }

    /// Mark `id` for repaint: the loop presents, and a client repaints and
    /// presents only the node's window-absolute bounds.
    pub(super) fn damage_node(&self, id: WidgetId) {
        self.dirty.store(true, Ordering::Relaxed);
        if self.is_client() {
            if let Some((window, bounds)) = self.absolute_damage(id) {
                self.add_damage(window, bounds);
            }
        }
    }

    pub(super) fn with_node<R>(&self, id: WidgetId, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        self.nodes
            .borrow_mut()
            .iter_mut()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| f(node))
    }
}
