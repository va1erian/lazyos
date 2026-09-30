//! Z-order: `Backend::raise`. Paint order and hit-test order are both the node
//! table's order (later is on top), so raising a node moves it, and its
//! descendants, to the end of the table.

use xui_core::backend::{ParentRef, WidgetId};

use super::{LazyOSBackend, Node};

/// `id` and its descendants, in table order.
pub(super) fn family(nodes: &[(WidgetId, Node)], id: WidgetId) -> Vec<WidgetId> {
    // Grow the set to a fixed point; the loop is bounded by the table length,
    // so a corrupted (cyclic) parent chain cannot spin.
    let mut raised = vec![id];
    for _ in 0..nodes.len() {
        let before = raised.len();
        for (node_id, node) in nodes.iter() {
            if let ParentRef::Widget(parent) = node.parent {
                if raised.contains(&parent) && !raised.contains(node_id) {
                    raised.push(*node_id);
                }
            }
        }
        if raised.len() == before {
            break;
        }
    }
    raised
}

/// Moves `id` and its descendants to the end of `nodes`, keeping their relative
/// order. Returns whether the table changed.
pub(super) fn raise_in(nodes: &mut Vec<(WidgetId, Node)>, id: WidgetId) -> bool {
    if !nodes.iter().any(|(node_id, _)| *node_id == id) {
        return false;
    }
    let raised = family(nodes, id);
    let (top, rest): (Vec<_>, Vec<_>) = std::mem::take(nodes)
        .into_iter()
        .partition(|(node_id, _)| raised.contains(node_id));
    *nodes = rest;
    nodes.extend(top);
    true
}

impl LazyOSBackend {
    /// Raises `id` above everything else in its window and repaints it.
    pub(super) fn raise_node(&self, id: WidgetId) {
        if self.is_client() {
            // Damage the node and every descendant, which may extend past the
            // node's own bounds.
            let members = family(&self.nodes.borrow(), id);
            for member in members {
                if let Some((window, area)) = self.absolute_damage(member) {
                    self.add_damage(window, area);
                }
            }
        }
        raise_in(&mut self.nodes.borrow_mut(), id);
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use xui_core::backend::WindowId;
    use xui_core::Rect;

    use super::*;

    fn id(raw: u64) -> WidgetId {
        WidgetId::from_raw(raw)
    }

    fn node(parent: ParentRef) -> Node {
        Node {
            window: WindowId::from_raw(1),
            parent,
            bounds: Rect::new(0, 0, 10, 10),
            visible: true,
            enabled: true,
            focus_stop: false,
            text: String::new(),
            painter: None,
            clip: None,
        }
    }

    fn order(nodes: &[(WidgetId, Node)]) -> Vec<u64> {
        nodes.iter().map(|(node_id, _)| node_id.raw()).collect()
    }

    fn table() -> Vec<(WidgetId, Node)> {
        let window = ParentRef::Window(WindowId::from_raw(1));
        vec![
            (id(1), node(window)),
            (id(2), node(ParentRef::Widget(id(1)))),
            (id(3), node(window)),
            (id(4), node(window)),
        ]
    }

    #[test]
    fn a_raised_node_moves_to_the_end() {
        let mut nodes = table();
        assert!(raise_in(&mut nodes, id(3)));
        assert_eq!(order(&nodes), [1, 2, 4, 3]);
    }

    #[test]
    fn descendants_travel_with_their_parent_in_order() {
        let mut nodes = table();
        assert!(raise_in(&mut nodes, id(1)));
        assert_eq!(order(&nodes), [3, 4, 1, 2]);
    }

    #[test]
    fn raising_the_top_node_or_an_unknown_id_changes_nothing() {
        let mut nodes = table();
        assert!(raise_in(&mut nodes, id(4)));
        assert_eq!(order(&nodes), [1, 2, 3, 4]);
        assert!(!raise_in(&mut nodes, id(99)));
        assert_eq!(order(&nodes), [1, 2, 3, 4]);
    }

    #[test]
    fn the_family_is_the_node_and_its_descendants() {
        let nodes = table();
        assert_eq!(family(&nodes, id(1)), [id(1), id(2)]);
        assert_eq!(family(&nodes, id(3)), [id(3)]);
    }

    #[test]
    fn a_cyclic_parent_chain_terminates() {
        let mut nodes = vec![
            (id(1), node(ParentRef::Widget(id(2)))),
            (id(2), node(ParentRef::Widget(id(1)))),
        ];
        assert!(raise_in(&mut nodes, id(1)));
        assert_eq!(order(&nodes), [1, 2]);
    }
}
