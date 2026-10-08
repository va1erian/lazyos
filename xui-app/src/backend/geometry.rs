//! Node geometry: window-absolute bounds, hit testing and event translation.
//!
//! A node's `bounds` are relative to its parent (as in `xui-canvas`'s offscreen
//! backend, whose traversal this mirrors), so anything that talks in window
//! pixels - hit tests, painting, pointer events - must first lift them by
//! walking the parent chain. Pure functions over the node table so they are
//! host-testable.

use xui_core::backend::{Event, ParentRef, WidgetId, WindowId};
use xui_core::{Point, Rect};

use super::Node;

/// The window-absolute bounds of `id`, or `None` when it (or an ancestor) is
/// missing.
pub(super) fn absolute_bounds(nodes: &[(WidgetId, Node)], id: WidgetId) -> Option<Rect> {
    let mut rect = node_of(nodes, id)?.bounds;
    let mut parent = node_of(nodes, id)?.parent;
    // A parent chain is shorter than the table; the bound stops a corrupted
    // (cyclic) chain from spinning forever.
    for _ in 0..=nodes.len() {
        match parent {
            ParentRef::Window(_) => return Some(rect),
            ParentRef::Widget(p) => {
                let ancestor = node_of(nodes, p)?;
                rect = rect.offset(ancestor.bounds.left, ancestor.bounds.top);
                parent = ancestor.parent;
            }
        }
    }
    None
}

fn node_of(nodes: &[(WidgetId, Node)], id: WidgetId) -> Option<&Node> {
    nodes
        .iter()
        .find(|(node_id, _)| *node_id == id)
        .map(|(_, node)| node)
}

/// Whether `id` and every ancestor satisfy `flag`; `false` if the chain is
/// broken.
fn ancestry_allows(nodes: &[(WidgetId, Node)], id: WidgetId, flag: fn(&Node) -> bool) -> bool {
    let mut current = ParentRef::Widget(id);
    for _ in 0..=nodes.len() {
        match current {
            ParentRef::Window(_) => return true,
            ParentRef::Widget(node_id) => {
                let Some(node) = node_of(nodes, node_id) else {
                    return false;
                };
                if !flag(node) {
                    return false;
                }
                current = node.parent;
            }
        }
    }
    false
}

/// Whether `id` is visible through its whole ancestry.
pub(super) fn effectively_visible(nodes: &[(WidgetId, Node)], id: WidgetId) -> bool {
    ancestry_allows(nodes, id, |node| node.visible)
}

/// The clip `id`'s ancestors impose, in window coordinates.
pub(super) fn ancestor_clip(nodes: &[(WidgetId, Node)], id: WidgetId) -> Option<Rect> {
    let mut clip: Option<Rect> = None;
    let mut parent = node_of(nodes, id)?.parent;
    for _ in 0..=nodes.len() {
        let ParentRef::Widget(p) = parent else { break };
        let (Some(ancestor), Some(origin)) = (node_of(nodes, p), absolute_bounds(nodes, p)) else {
            break;
        };
        if let Some(local) = ancestor.clip {
            let rect = Rect::new(
                origin.left + local.left,
                origin.top + local.top,
                origin.left + local.right,
                origin.top + local.bottom,
            );
            clip = Some(clip.map_or(rect, |c| intersect(c, rect)));
        }
        parent = ancestor.parent;
    }
    clip
}

fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

/// The topmost node in `window` under window point `(x, y)` with its
/// window-absolute bounds. The node and its ancestors must be visible and
/// enabled, and the point inside every ancestor clip.
pub(super) fn hit(
    nodes: &[(WidgetId, Node)],
    window: WindowId,
    x: i32,
    y: i32,
) -> Option<(WidgetId, Rect)> {
    let point = Point::new(x, y);
    nodes.iter().rev().find_map(|(id, node)| {
        if node.window != window
            || !effectively_visible(nodes, *id)
            || !ancestry_allows(nodes, *id, |n| n.enabled)
        {
            return None;
        }
        let abs = absolute_bounds(nodes, *id)?;
        let clipped = ancestor_clip(nodes, *id).is_some_and(|clip| !clip.contains(point));
        (abs.contains(point) && !clipped).then_some((*id, abs))
    })
}

/// Rebuilds a pointer `event` with its position at `(x, y)`; other events pass
/// through unchanged.
pub(super) fn translate(event: Event, x: i32, y: i32) -> Event {
    match event {
        Event::MouseDown {
            button, modifiers, ..
        } => Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        },
        Event::MouseUp {
            button, modifiers, ..
        } => Event::MouseUp {
            x,
            y,
            button,
            modifiers,
        },
        Event::MouseMove { modifiers, .. } => Event::MouseMove { x, y, modifiers },
        Event::MouseDoubleClick {
            button, modifiers, ..
        } => Event::MouseDoubleClick {
            x,
            y,
            button,
            modifiers,
        },
        Event::MouseWheel {
            delta,
            horizontal,
            modifiers,
            ..
        } => Event::MouseWheel {
            delta,
            horizontal,
            x,
            y,
            modifiers,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_core::{Modifiers, MouseButton};

    const W: WindowId = WindowId::from_raw(1);

    fn node(parent: ParentRef, bounds: Rect) -> Node {
        Node {
            window: W,
            parent,
            bounds,
            visible: true,
            enabled: true,
            focus_stop: false,
            wants_tab: false,
            text: String::new(),
            painter: None,
            clip: None,
        }
    }

    fn id(raw: u64) -> WidgetId {
        WidgetId::from_raw(raw)
    }

    /// Node 1: a toolbar-offset panel at (0,27); node 2: a child at (5,3).
    fn table() -> Vec<(WidgetId, Node)> {
        vec![
            (
                id(1),
                node(ParentRef::Window(W), Rect::new(0, 27, 200, 127)),
            ),
            (
                id(2),
                node(ParentRef::Widget(id(1)), Rect::new(5, 3, 55, 43)),
            ),
        ]
    }

    #[test]
    fn absolute_bounds_walk_the_parent_chain() {
        let nodes = table();
        assert_eq!(
            absolute_bounds(&nodes, id(2)),
            Some(Rect::new(5, 30, 55, 70))
        );
        assert_eq!(absolute_bounds(&nodes, id(9)), None);
    }

    #[test]
    fn a_missing_ancestor_or_cycle_has_no_bounds() {
        let orphan = vec![(id(2), node(ParentRef::Widget(id(7)), Rect::new(0, 0, 1, 1)))];
        assert_eq!(absolute_bounds(&orphan, id(2)), None);
        let cycle = vec![
            (id(1), node(ParentRef::Widget(id(2)), Rect::new(0, 0, 1, 1))),
            (id(2), node(ParentRef::Widget(id(1)), Rect::new(0, 0, 1, 1))),
        ];
        assert_eq!(absolute_bounds(&cycle, id(1)), None);
    }

    #[test]
    fn hit_uses_absolute_bounds_and_topmost_child() {
        let nodes = table();
        // In the child (absolute 5..55 x 30..70).
        assert_eq!(
            hit(&nodes, W, 10, 35),
            Some((id(2), Rect::new(5, 30, 55, 70)))
        );
        // In the panel only.
        assert_eq!(hit(&nodes, W, 100, 100).map(|h| h.0), Some(id(1)));
        // The raw (parent-relative) child bounds must not hit: (10, 5) is
        // above the panel entirely.
        assert_eq!(hit(&nodes, W, 10, 5), None);
        // Another window never hits.
        assert_eq!(hit(&nodes, WindowId::from_raw(2), 10, 35), None);
    }

    #[test]
    fn hidden_or_disabled_ancestors_hide_children() {
        let mut nodes = table();
        nodes[0].1.visible = false;
        assert_eq!(hit(&nodes, W, 10, 35), None);
        let mut nodes = table();
        nodes[0].1.enabled = false;
        assert_eq!(hit(&nodes, W, 10, 35), None);
    }

    #[test]
    fn an_ancestor_clip_limits_hits() {
        let mut nodes = table();
        // Clip the panel's children to its left 20 px (panel coordinates).
        nodes[0].1.clip = Some(Rect::new(0, 0, 20, 100));
        assert_eq!(hit(&nodes, W, 10, 35).map(|h| h.0), Some(id(2)));
        // Inside the child but outside the clip: falls through to the panel.
        assert_eq!(hit(&nodes, W, 40, 35).map(|h| h.0), Some(id(1)));
    }

    #[test]
    fn translate_rewrites_only_pointer_positions() {
        let m = Modifiers::NONE;
        assert_eq!(
            translate(
                Event::MouseDown {
                    x: 50,
                    y: 60,
                    button: MouseButton::Left,
                    modifiers: m
                },
                3,
                4
            ),
            Event::MouseDown {
                x: 3,
                y: 4,
                button: MouseButton::Left,
                modifiers: m
            }
        );
        assert_eq!(
            translate(
                Event::MouseMove {
                    x: 1,
                    y: 2,
                    modifiers: m
                },
                -5,
                -6
            ),
            Event::MouseMove {
                x: -5,
                y: -6,
                modifiers: m
            }
        );
        assert_eq!(
            translate(Event::CaptureChanged, 1, 1),
            Event::CaptureChanged
        );
    }
}
