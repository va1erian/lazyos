//! Moving a batch of nodes (`Backend::apply_moves`). A layout re-flow sends
//! the widgets it placed, most of them where they already are: those cost
//! nothing here, and the others are found through one index instead of a
//! table scan each (va1erian/xui#277).

use std::collections::{HashMap, HashSet};

use xui_core::backend::{ParentRef, WidgetId};
use xui_core::Rect;

use super::geometry::absolute_bounds;
use super::Node;

/// What a batch of moves changed.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Moved {
    /// The nodes whose size changed, at their new bounds: they get a `Resize`.
    pub resized: Vec<(WidgetId, Rect)>,
    /// The window-absolute area to repaint: the old and the new position of
    /// every moved node and of its descendants (a child may extend past its
    /// parent). `None` when nothing moved or damage is not tracked.
    pub damage: Option<Rect>,
}

/// Moves each node of `moves` in `nodes` to its rect (parent-relative),
/// skipping nodes already there and ids not in the table. Damage is only
/// computed when `track_damage` (a client window; owner mode repaints whole).
pub(super) fn apply(
    nodes: &mut [(WidgetId, Node)],
    moves: &[(WidgetId, Rect)],
    track_damage: bool,
) -> Moved {
    let mut moved = Moved::default();
    if moves.is_empty() {
        return moved;
    }
    let index: HashMap<u64, usize> = nodes
        .iter()
        .enumerate()
        .map(|(at, (id, _))| (id.raw(), at))
        .collect();
    // Built on the first real move: a batch only moves nodes, so the tree's
    // shape holds for the whole batch.
    let mut children: Option<HashMap<u64, Vec<WidgetId>>> = None;
    for (id, rect) in moves {
        let Some(&at) = index.get(&id.raw()) else {
            continue;
        };
        let old = nodes[at].1.bounds;
        if old == *rect {
            continue;
        }
        if old.size() != rect.size() {
            moved.resized.push((*id, *rect));
        }
        if !track_damage {
            nodes[at].1.bounds = *rect;
            continue;
        }
        let family = family(children.get_or_insert_with(|| children_of(nodes)), *id);
        let before = union_bounds(nodes, &family);
        nodes[at].1.bounds = *rect;
        let after = union_bounds(nodes, &family);
        for area in [before, after].into_iter().flatten() {
            moved.damage = Some(moved.damage.map_or(area, |damage| union(damage, area)));
        }
    }
    moved
}

/// Each widget parent's children, by the parent's raw id.
fn children_of(nodes: &[(WidgetId, Node)]) -> HashMap<u64, Vec<WidgetId>> {
    let mut children: HashMap<u64, Vec<WidgetId>> = HashMap::new();
    for (id, node) in nodes {
        if let ParentRef::Widget(parent) = node.parent {
            children.entry(parent.raw()).or_default().push(*id);
        }
    }
    children
}

/// `id` and every node below it. The walk visits each node once (a big list
/// or tree stays linear), so a corrupted (cyclic) parent chain cannot spin.
fn family(children: &HashMap<u64, Vec<WidgetId>>, id: WidgetId) -> Vec<WidgetId> {
    let mut family = vec![id];
    let mut seen = HashSet::from([id.raw()]);
    let mut next = 0;
    while next < family.len() {
        for child in children.get(&family[next].raw()).into_iter().flatten() {
            if seen.insert(child.raw()) {
                family.push(*child);
            }
        }
        next += 1;
    }
    family
}

/// The window-absolute rectangle covering every node of `family`.
fn union_bounds(nodes: &[(WidgetId, Node)], family: &[WidgetId]) -> Option<Rect> {
    family
        .iter()
        .filter_map(|member| absolute_bounds(nodes, *member))
        .reduce(union)
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

#[cfg(test)]
mod tests {
    use xui_core::backend::WindowId;

    use super::*;

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
            clip: None,
            text: String::new(),
            painter: None,
        }
    }

    /// A panel at (10, 10) holding a button, and a label beside the panel.
    fn table() -> Vec<(WidgetId, Node)> {
        let panel = WidgetId::from_raw(1);
        vec![
            (
                panel,
                node(ParentRef::Window(W), Rect::new(10, 10, 110, 60)),
            ),
            (
                WidgetId::from_raw(2),
                node(ParentRef::Widget(panel), Rect::new(5, 5, 45, 25)),
            ),
            (
                WidgetId::from_raw(3),
                node(ParentRef::Window(W), Rect::new(120, 10, 180, 30)),
            ),
        ]
    }

    #[test]
    fn a_batch_that_moves_nothing_damages_and_resizes_nothing() {
        let mut nodes = table();
        let same: Vec<_> = nodes.iter().map(|(id, node)| (*id, node.bounds)).collect();
        assert_eq!(apply(&mut nodes, &same, true), Moved::default());
    }

    #[test]
    fn a_moved_parent_damages_its_old_and_new_place_with_its_children() {
        let mut nodes = table();
        let moved = apply(
            &mut nodes,
            &[
                (WidgetId::from_raw(1), Rect::new(10, 100, 110, 150)),
                (WidgetId::from_raw(3), Rect::new(120, 10, 180, 30)),
            ],
            true,
        );
        assert!(moved.resized.is_empty(), "same size: no resize");
        assert_eq!(moved.damage, Some(Rect::new(10, 10, 110, 150)));
        assert_eq!(nodes[0].1.bounds, Rect::new(10, 100, 110, 150));
    }

    #[test]
    fn a_resized_node_is_reported_and_unknown_ids_are_ignored() {
        let mut nodes = table();
        let wider = Rect::new(120, 10, 200, 30);
        let moved = apply(
            &mut nodes,
            &[
                (WidgetId::from_raw(3), wider),
                (WidgetId::from_raw(99), Rect::new(0, 0, 5, 5)),
            ],
            false,
        );
        assert_eq!(moved.resized, vec![(WidgetId::from_raw(3), wider)]);
        assert_eq!(moved.damage, None, "owner mode tracks no damage");
        assert_eq!(nodes[2].1.bounds, wider);
    }

    #[test]
    fn a_cyclic_parent_chain_ends_the_family_walk() {
        let mut nodes = table();
        nodes[0].1.parent = ParentRef::Widget(WidgetId::from_raw(2));
        let moved = apply(
            &mut nodes,
            &[(WidgetId::from_raw(1), Rect::new(0, 0, 100, 50))],
            true,
        );
        // A cycle has no absolute bounds; the walk still finishes.
        assert_eq!(moved.damage, None);
    }
}
