//! The Explorer's model: the tree rows over the catalog, the Live and Topics
//! tables, the Gallery list (`docs/messenger-playground-plan.md` section 3).
//!
//! Plain data in, rows out: the window owns the widgets, this owns what they
//! show, so a filter or a refresh is a function from state to rows. Nothing
//! here reads the fabric; [`crate::app`] hands in the registry and topic rows.

use std::collections::HashSet;
use std::rc::Rc;

use xui_app::fabric::{RegistryEntry, TopicRow as BrokerTopic};
use xui_core::widget::TreeRow;
use xui_playground::catalog::{Catalog, Kind, Node, NodeId};
use xui_playground::gallery::{self, Example};
use xui_playground::live::{LiveJoin, RegistryRow, TopicEntry, TopicRow};

/// How many hits a search keeps.
const MAX_HITS: usize = 400;

/// What the Explorer shows and how it is filtered.
pub struct Explorer {
    pub catalog: Rc<Catalog>,
    pub join: LiveJoin,
    registry: Vec<RegistryRow>,
    pub topics: Vec<TopicEntry>,
    /// The broker's topic list, kept to rebuild the merged table.
    broker: Vec<TopicRow>,
    pub examples: Vec<Example>,
    /// The Interfaces tab's search text.
    pub query: String,
    visible: Option<HashSet<NodeId>>,
    expanded: HashSet<usize>,
    /// Flat tree row to catalog node index (what `NodeId` of the widget means).
    rows: Vec<usize>,
    live_signature: u64,
    pub live_filter: String,
    pub topic_filter: String,
    pub gallery_filter: String,
    /// Rows of the tables as last built, to map a selection back.
    pub live_names: Vec<String>,
    pub topic_labels: Vec<String>,
    pub gallery_ids: Vec<&'static str>,
}

impl Explorer {
    pub fn new() -> Explorer {
        let catalog = Rc::new(Catalog::from_schema());
        Explorer {
            catalog,
            join: LiveJoin::default(),
            registry: Vec::new(),
            topics: Vec::new(),
            broker: Vec::new(),
            examples: gallery::examples(),
            query: String::new(),
            visible: None,
            expanded: HashSet::new(),
            rows: Vec::new(),
            live_signature: 0,
            live_filter: String::new(),
            topic_filter: String::new(),
            gallery_filter: String::new(),
            live_names: Vec::new(),
            topic_labels: Vec::new(),
            gallery_ids: Vec::new(),
        }
    }

    /// Take the registry's rows. True when the set of live interfaces
    /// changed, so the tree's badges need redrawing.
    pub fn set_registry(&mut self, entries: &[RegistryEntry]) -> bool {
        self.registry = entries
            .iter()
            .map(|e| RegistryRow {
                name: e.name.clone(),
                object: e.object_id,
                owner: e.owner_slot,
                interfaces: e.interfaces.clone(),
                lease_remaining: e.lease_remaining,
                owner_uid: e.owner_uid,
                owner_label: e.owner_label,
                owner_session: e.owner_session,
            })
            .collect();
        self.join = self.catalog.join_live(&self.registry);
        self.topics = self.catalog.join_topics(&self.broker);
        let mut signature = 0u64;
        for interface in self.catalog.interfaces() {
            if self.join.is_live(interface) {
                signature = signature.wrapping_mul(31).wrapping_add(interface.id | 1);
            }
        }
        let changed = signature != self.live_signature;
        self.live_signature = signature;
        changed
    }

    /// Take the broker's topic list (a user-driven read).
    pub fn set_broker_topics(&mut self, rows: &[BrokerTopic]) {
        self.broker = rows
            .iter()
            .map(|t| TopicRow {
                topic: t.topic.clone(),
                subscribers: t.subscribers,
                retained: t.retained,
            })
            .collect();
        self.topics = self.catalog.join_topics(&self.broker);
    }

    // ---- the Interfaces tree -------------------------------------------

    /// Set the search text. The tree shows the hits and their ancestors,
    /// expanded; an empty text shows everything, collapsed as the user left it.
    pub fn set_query(&mut self, text: &str) {
        self.query = text.trim().to_string();
        self.visible = if self.query.is_empty() {
            None
        } else {
            let hits = self.catalog.search(&self.query, MAX_HITS);
            Some(self.catalog.visible_for(&hits))
        };
    }

    /// Remember a toggle of the interface in flat row `row`.
    pub fn toggled(&mut self, row: usize, expanded: bool) {
        if let Some(&index) = self.rows.get(row) {
            if expanded {
                self.expanded.insert(index);
            } else {
                self.expanded.remove(&index);
            }
        }
    }

    /// The tree rows now (and the row-to-node map they use).
    pub fn tree_rows(&mut self) -> Vec<TreeRow> {
        let searching = self.visible.is_some();
        let mut rows = Vec::new();
        let mut map = Vec::new();
        let nodes = self.catalog.nodes();
        let roots: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.parent.is_none())
            .map(|(i, _)| i)
            .collect();
        for root in roots {
            let node = &nodes[root];
            if !self.shown(node) {
                continue;
            }
            let live = match node.item {
                xui_playground::catalog::Item::Interface(i) => self.join.is_live(i),
                _ => false,
            };
            let label = if live {
                format!("{}  [LIVE]", node.label)
            } else {
                node.label.clone()
            };
            let children: Vec<usize> = node
                .children
                .iter()
                .copied()
                .filter(|&c| self.shown(&nodes[c]))
                .collect();
            let open = searching || self.expanded.contains(&root);
            rows.push(
                TreeRow::new(label, 0)
                    .expandable(!children.is_empty())
                    .expanded(open),
            );
            map.push(root);
            for child in children {
                rows.push(TreeRow::new(child_label(&nodes[child]), 1));
                map.push(child);
            }
        }
        self.rows = map;
        rows
    }

    fn shown(&self, node: &Node) -> bool {
        self.visible
            .as_ref()
            .is_none_or(|set| set.contains(&node.id))
    }

    /// The catalog node a flat tree row is.
    pub fn node_at(&self, row: usize) -> Option<&Node> {
        self.rows.get(row).map(|&i| &self.catalog.nodes()[i])
    }

    /// The flat row of `id` (to select it after a jump), if it is shown.
    pub fn row_of(&self, id: &NodeId) -> Option<usize> {
        self.rows
            .iter()
            .position(|&i| &self.catalog.nodes()[i].id == id)
    }

    /// Make sure `id`'s interface is expanded so its row exists.
    pub fn reveal(&mut self, id: &NodeId) {
        if let Some(node) = self.catalog.get(id) {
            let interface = node.item.interface().name;
            if let Some(root) = self.catalog.interface_node(interface) {
                let index = node_index(self.catalog.nodes(), root);
                self.expanded.insert(index);
            }
        }
    }

    // ---- the tables ----------------------------------------------------

    /// The Live tab: name, interfaces, owner uid, label, session, lease.
    pub fn live_table(&mut self) -> Vec<Vec<String>> {
        let shown: Vec<Vec<String>> = self
            .join
            .filter(&self.live_filter)
            .into_iter()
            .map(|s| {
                vec![
                    s.row.name.clone(),
                    s.interfaces
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    s.row.owner_uid.to_string(),
                    s.label_text(),
                    s.session_text(),
                    s.lease_text(),
                ]
            })
            .collect();
        self.live_names = shown.iter().map(|r| r[0].clone()).collect();
        shown
    }

    /// The first known interface of the live service called `name`.
    pub fn interface_of_service(&self, name: &str) -> Option<&'static str> {
        self.join.service(name).and_then(|s| {
            s.interfaces.iter().find_map(|i| match i {
                xui_playground::live::IfaceRef::Known(k) => Some(k.name),
                _ => None,
            })
        })
    }

    /// The Topics tab: pattern or topic, payload type, QoS, retained,
    /// subscribers, permissions.
    pub fn topic_table(&mut self) -> Vec<Vec<String>> {
        let needle = self.topic_filter.to_lowercase();
        let shown: Vec<&TopicEntry> = self
            .topics
            .iter()
            .filter(|t| needle.is_empty() || t.label.to_lowercase().contains(&needle))
            .collect();
        self.topic_labels = shown.iter().map(|t| t.label.clone()).collect();
        shown
            .into_iter()
            .map(|t| {
                vec![
                    t.label.clone(),
                    t.payload().to_string(),
                    t.qos().unwrap_or("-").to_string(),
                    if t.retained { "yes" } else { "" }.to_string(),
                    t.subscribers.to_string(),
                    t.permissions.join(" "),
                ]
            })
            .collect()
    }

    /// The topic entry listed in row `row` of the table last built.
    pub fn topic_entry(&self, label: &str) -> Option<&TopicEntry> {
        self.topics.iter().find(|t| t.label == label)
    }

    /// The Gallery tab: number, title, needs.
    pub fn gallery_table(&mut self) -> Vec<Vec<String>> {
        let needle = self.gallery_filter.to_lowercase();
        let shown: Vec<&Example> = self
            .examples
            .iter()
            .filter(|e| {
                needle.is_empty()
                    || e.title.to_lowercase().contains(&needle)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&needle))
            })
            .collect();
        self.gallery_ids = shown.iter().map(|e| e.id).collect();
        shown
            .into_iter()
            .map(|e| {
                vec![
                    e.number().to_string(),
                    e.title.clone(),
                    e.needs
                        .iter()
                        .map(|n| n.label())
                        .collect::<Vec<_>>()
                        .join(", "),
                ]
            })
            .collect()
    }

    pub fn example(&self, id: &str) -> Option<&Example> {
        self.examples.iter().find(|e| e.id == id)
    }
}

fn node_index(nodes: &[Node], node: &Node) -> usize {
    nodes
        .iter()
        .position(|n| n.id == node.id)
        .unwrap_or_default()
}

/// A child row: the kind, then the name.
fn child_label(node: &Node) -> String {
    let prefix = match node.kind() {
        Kind::Interface => "",
        Kind::Method => "fn ",
        Kind::Struct => "struct ",
        Kind::Enum => "enum ",
        Kind::Topic => "topic ",
    };
    format!("{prefix}{}", node.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_lists_every_interface_collapsed_and_a_search_expands_the_hits() {
        let mut e = Explorer::new();
        let all = e.tree_rows();
        assert!(all.len() > 10);
        assert!(all.iter().all(|r| r.depth == 0 || r.depth == 1));
        e.set_query("confd get");
        let hits = e.tree_rows();
        assert!(hits.len() < all.len());
        assert!(hits.iter().any(|r| r.label.contains("confd")));
        assert!(hits.iter().any(|r| r.expanded));
        e.set_query("");
        assert_eq!(e.tree_rows().len(), all.len());
    }

    #[test]
    fn a_row_maps_back_to_its_node() {
        let mut e = Explorer::new();
        let rows = e.tree_rows();
        let first = e.node_at(0).unwrap().id.clone();
        assert!(rows[0].label.starts_with(&first.0));
        assert_eq!(e.row_of(&first), Some(0));
    }

    #[test]
    fn the_gallery_table_lists_the_examples() {
        let mut e = Explorer::new();
        let table = e.gallery_table();
        assert_eq!(table.len(), e.gallery_ids.len());
        assert!(table.len() >= 13);
        e.gallery_filter = "toast".into();
        assert!(!e.gallery_table().is_empty() && e.gallery_table().len() < table.len());
    }
}
