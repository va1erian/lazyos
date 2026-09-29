#![forbid(unsafe_code)]

//! Node geometry traversal for the offscreen backend: a node's window-absolute
//! bounds and the clip its ancestors impose.
//!
//! This was upstream `backend/geometry.rs`, shared with the windowed compositor;
//! the LazyOS vendored copy drops the windowed backend (see README) and keeps
//! the offscreen walk alone.

use xui_core::backend::{ParentRef, WidgetId};
use xui_core::geometry::{Point, Rect};

/// The placement facts the geometry traversal needs from a node.
pub(crate) trait GeometryNode {
    /// The node this one is placed relative to.
    fn parent(&self) -> ParentRef;
    /// This node's bounds, relative to its parent.
    fn bounds(&self) -> Rect;
    /// A clip on this node's descendants, in this node's own coordinate space.
    fn clip(&self) -> Option<Rect>;
    /// Whether this node itself is visible (ignoring its ancestors).
    fn visible(&self) -> bool;
    /// Whether this node itself is enabled (ignoring its ancestors).
    fn enabled(&self) -> bool;
}

/// The window-absolute bounds of `id`, walking its parent chain.
pub(crate) fn absolute_bounds<N: GeometryNode>(
    nodes: &[(WidgetId, N)],
    id: WidgetId,
) -> Option<Rect> {
    let (_, node) = nodes.iter().find(|(node_id, _)| *node_id == id)?;
    Some(match node.parent() {
        ParentRef::Window(_) => node.bounds(),
        ParentRef::Widget(parent) => {
            let outer = absolute_bounds(nodes, parent)?;
            node.bounds().offset(outer.left, outer.top)
        }
    })
}

/// The intersection of two rectangles.
pub(crate) fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

/// The clip on `id`'s painting from its ancestors' [`GeometryNode::clip`]s, in
/// window coordinates, or `None` when no ancestor clips it.
///
/// A node's clip is in that node's own coordinate space, so it is lifted to
/// window coordinates with the node's absolute origin; descendants intersect
/// every ancestor's clip.
pub(crate) fn ancestor_clip<N: GeometryNode>(
    nodes: &[(WidgetId, N)],
    id: WidgetId,
) -> Option<Rect> {
    let (_, node) = nodes.iter().find(|(node_id, _)| *node_id == id)?;
    let mut clip: Option<Rect> = None;
    let mut parent = node.parent();
    while let ParentRef::Widget(parent_id) = parent {
        let Some((_, ancestor)) = nodes.iter().find(|(node_id, _)| *node_id == parent_id) else {
            break;
        };
        let Some(origin) = absolute_bounds(nodes, parent_id) else {
            break;
        };
        if let Some(local) = ancestor.clip() {
            let rect = Rect::new(
                origin.left + local.left,
                origin.top + local.top,
                origin.left + local.right,
                origin.top + local.bottom,
            );
            clip = Some(match clip {
                Some(current) => intersect(current, rect),
                None => rect,
            });
        }
        parent = ancestor.parent();
    }
    clip
}

/// Whether `id` is visible through its whole ancestry: its own flag and every
/// ancestor's. A node under a hidden ancestor is not composited, matching the
/// Win32 child-window cascade.
pub(crate) fn effectively_visible<N: GeometryNode>(nodes: &[(WidgetId, N)], id: WidgetId) -> bool {
    ancestry_allows(nodes, id, GeometryNode::visible)
}

/// Whether `id` is enabled through its whole ancestry: its own flag and every
/// ancestor's. A disabled container disables its children, as a disabled parent
/// window does on Win32.
pub(crate) fn effectively_enabled<N: GeometryNode>(nodes: &[(WidgetId, N)], id: WidgetId) -> bool {
    ancestry_allows(nodes, id, GeometryNode::enabled)
}

/// Whether `id` and every ancestor satisfy `flag`, or `false` when a node on
/// the chain is missing.
fn ancestry_allows<N: GeometryNode>(
    nodes: &[(WidgetId, N)],
    id: WidgetId,
    flag: impl Fn(&N) -> bool,
) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        let Some((_, node)) = nodes.iter().find(|(candidate, _)| *candidate == node_id) else {
            return false;
        };
        if !flag(node) {
            return false;
        }
        current = match node.parent() {
            ParentRef::Window(_) => None,
            ParentRef::Widget(parent) => Some(parent),
        };
    }
    true
}

/// The window-absolute bounds of `id` for hit-testing `(x, y)`, or `None` when
/// `id` is not [effectively visible](effectively_visible) and
/// [enabled](effectively_enabled), the point is outside those bounds, or it
/// falls outside an ancestor's [`GeometryNode::clip`]. The windowed and
/// offscreen hit tests share this so both agree with the paint path.
pub(crate) fn hit_bounds<N: GeometryNode>(
    nodes: &[(WidgetId, N)],
    id: WidgetId,
    x: i32,
    y: i32,
) -> Option<Rect> {
    if !effectively_visible(nodes, id) || !effectively_enabled(nodes, id) {
        return None;
    }
    let bounds = absolute_bounds(nodes, id)?;
    let point = Point::new(x, y);
    if !bounds.contains(point) {
        return None;
    }
    if let Some(clip) = ancestor_clip(nodes, id)
        && !clip.contains(point)
    {
        return None;
    }
    Some(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_core::backend::WindowId;

    struct TestNode {
        parent: ParentRef,
        bounds: Rect,
        clip: Option<Rect>,
        visible: bool,
        enabled: bool,
    }

    impl GeometryNode for TestNode {
        fn parent(&self) -> ParentRef {
            self.parent
        }

        fn bounds(&self) -> Rect {
            self.bounds
        }

        fn clip(&self) -> Option<Rect> {
            self.clip
        }

        fn visible(&self) -> bool {
            self.visible
        }

        fn enabled(&self) -> bool {
            self.enabled
        }
    }

    fn node(parent: ParentRef, bounds: Rect) -> TestNode {
        TestNode {
            parent,
            bounds,
            clip: None,
            visible: true,
            enabled: true,
        }
    }

    #[test]
    fn visibility_cascades_through_ancestors() {
        let window = WindowId::from_raw(1);
        let panel = WidgetId::from_raw(10);
        let child = WidgetId::from_raw(11);
        let mut panel_node = node(ParentRef::Window(window), Rect::new(0, 0, 50, 50));
        panel_node.visible = false;
        let nodes = vec![
            (panel, panel_node),
            (
                child,
                node(ParentRef::Widget(panel), Rect::new(5, 5, 10, 10)),
            ),
        ];

        assert!(
            !effectively_visible(&nodes, child),
            "the hidden panel hides its child"
        );
    }

    #[test]
    fn enabled_cascades_through_ancestors() {
        let window = WindowId::from_raw(1);
        let panel = WidgetId::from_raw(10);
        let child = WidgetId::from_raw(11);
        let mut panel_node = node(ParentRef::Window(window), Rect::new(0, 0, 50, 50));
        panel_node.enabled = false;
        let nodes = vec![
            (panel, panel_node),
            (
                child,
                node(ParentRef::Widget(panel), Rect::new(5, 5, 10, 10)),
            ),
        ];

        assert!(
            effectively_visible(&nodes, child),
            "visibility is unaffected"
        );
        assert!(
            !effectively_enabled(&nodes, child),
            "the disabled panel disables its child"
        );
    }

    #[test]
    fn hit_testing_rejects_a_child_under_a_hidden_ancestor() {
        let window = WindowId::from_raw(1);
        let panel = WidgetId::from_raw(10);
        let child = WidgetId::from_raw(11);
        let mut panel_node = node(ParentRef::Window(window), Rect::new(0, 0, 50, 50));
        panel_node.visible = false;
        let nodes = vec![
            (panel, panel_node),
            (
                child,
                node(ParentRef::Widget(panel), Rect::new(5, 5, 10, 10)),
            ),
        ];

        assert!(
            hit_bounds(&nodes, child, 8, 8).is_none(),
            "the hidden ancestor's child is not a hit target"
        );
    }

    #[test]
    fn hit_testing_observes_ancestor_clips() {
        let window = WindowId::from_raw(1);
        let view = WidgetId::from_raw(20);
        let child = WidgetId::from_raw(21);
        let mut view_node = node(ParentRef::Window(window), Rect::new(0, 0, 100, 80));
        // The view clips its descendants to a 10px band at its top.
        view_node.clip = Some(Rect::new(0, 0, 100, 10));
        let nodes = vec![
            (view, view_node),
            (
                child,
                node(ParentRef::Widget(view), Rect::new(0, 0, 100, 40)),
            ),
        ];

        assert!(
            hit_bounds(&nodes, child, 50, 5).is_some(),
            "inside the clip"
        );
        assert!(
            hit_bounds(&nodes, child, 50, 20).is_none(),
            "below the clip is not a hit target"
        );
    }
}
