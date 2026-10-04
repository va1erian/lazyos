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

    /// Mark `rect` of `id` for repaint: node-relative, as on Win32, and
    /// clipped to the node. Owner mode repaints the whole screen anyway.
    pub(super) fn damage_node_rect(&self, id: WidgetId, rect: Rect) {
        self.dirty.store(true, Ordering::Relaxed);
        if !self.is_client() {
            return;
        }
        let Some((window, bounds)) = self.absolute_damage(id) else {
            return;
        };
        let moved = rect.offset(bounds.left, bounds.top);
        let clipped = Rect::new(
            moved.left.max(bounds.left),
            moved.top.max(bounds.top),
            moved.right.min(bounds.right),
            moved.bottom.min(bounds.bottom),
        );
        if !clipped.is_empty() {
            self.add_damage(window, clipped);
        }
    }

    /// While painters run, the window rectangle being repainted: only its
    /// pixels reach the frame, so a painter may skip what lies outside it
    /// (the Terminal draws only the rows inside it). `None` outside a paint.
    pub fn paint_damage(&self) -> Option<Rect> {
        self.paint_damage.get()
    }

    pub(super) fn with_node<R>(&self, id: WidgetId, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        self.nodes
            .borrow_mut()
            .iter_mut()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| f(node))
    }
}
