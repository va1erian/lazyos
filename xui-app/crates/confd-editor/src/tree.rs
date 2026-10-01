//! The pure tree model over the flat, sorted path list `List` returns.
//!
//! `confd` stores a flat set of paths; the editor shows them as a folder tree.
//! This module owns that projection, the expand/collapse state (keyed by full
//! path) and the flattening a [`ListView`](xui_core::ListView) draws. It holds
//! no values and touches no store, so it is fully host-testable.

use std::collections::{BTreeMap, BTreeSet};

/// One visible line of the flattened tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The full path this row stands for (the key to select or toggle).
    pub path: String,
    /// The last path segment, the text shown after the indent.
    pub name: String,
    /// Nesting depth, used for indentation.
    pub depth: usize,
    /// Whether this path has children in the current view.
    pub folder: bool,
    /// Whether this path is a stored key (a path may be both).
    pub leaf: bool,
    /// Whether a folder is currently expanded.
    pub expanded: bool,
}

impl Row {
    /// The indented, marker-prefixed text a `ListView` draws.
    pub fn label(&self) -> String {
        let indent = "  ".repeat(self.depth);
        let marker = if self.folder {
            if self.expanded {
                "- "
            } else {
                "+ "
            }
        } else {
            "  "
        };
        format!("{indent}{marker}{}", self.name)
    }
}

/// An intermediate node while the flat list is folded into a tree.
#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    leaf: bool,
}

/// The tree of every listed path, with the user's expansion state.
#[derive(Default)]
pub struct Tree {
    paths: Vec<String>,
    expanded: BTreeSet<String>,
}

impl Tree {
    /// A tree over `paths` (unsorted input is fine).
    pub fn new(paths: Vec<String>) -> Tree {
        let mut tree = Tree::default();
        tree.refresh(paths);
        tree
    }

    /// Replaces the paths and drops expansion state for folders that are gone.
    ///
    /// The editor calls this after every `List`, so a key another actor removed
    /// leaves no stale expansion behind, while surviving folders keep theirs.
    pub fn refresh(&mut self, paths: Vec<String>) {
        let mut paths: Vec<String> = paths.into_iter().filter(|path| !path.is_empty()).collect();
        paths.sort();
        paths.dedup();
        self.paths = paths;
        let folders = self.folder_set();
        self.expanded.retain(|path| folders.contains(path));
    }

    /// Every stored path, sorted.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Whether `path` is expanded.
    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    /// Toggles `path`'s expansion (callers check [`Row::folder`] first).
    pub fn toggle(&mut self, path: &str) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_owned());
        }
    }

    /// The flattened visible rows, indented by depth.
    ///
    /// `filter` matches leaf paths by substring; ancestors are kept so a match
    /// is always reachable. While filtering, every folder is expanded, so a
    /// match is never hidden under a collapsed ancestor.
    pub fn rows(&self, filter: &str) -> Vec<Row> {
        let root = self.nodes_for(filter);
        let expanded = if filter.is_empty() {
            self.expanded.clone()
        } else {
            folders_of(&root)
        };
        let mut rows = Vec::new();
        walk(&root, "", 0, &expanded, &mut rows);
        rows
    }

    /// The paths with children under the current path set.
    fn folder_set(&self) -> BTreeSet<String> {
        folders_of(&self.nodes_for(""))
    }

    /// The folded tree for `filter` (all paths when the filter is empty).
    fn nodes_for(&self, filter: &str) -> Node {
        if filter.is_empty() {
            build_nodes(self.paths.iter().map(String::as_str))
        } else {
            build_nodes(
                self.paths
                    .iter()
                    .filter(|path| path.contains(filter))
                    .map(String::as_str),
            )
        }
    }
}

/// Folds full paths into a tree, creating a folder node per segment.
fn build_nodes<'a>(paths: impl Iterator<Item = &'a str>) -> Node {
    let mut root = Node::default();
    for path in paths {
        let mut node = &mut root;
        for segment in path.split('/') {
            node = node.children.entry(segment.to_owned()).or_default();
        }
        node.leaf = true;
    }
    root
}

/// Every path in `root` that has at least one child.
fn folders_of(root: &Node) -> BTreeSet<String> {
    let mut folders = BTreeSet::new();
    collect_folders(root, "", &mut folders);
    folders
}

fn collect_folders(node: &Node, prefix: &str, out: &mut BTreeSet<String>) {
    for (name, child) in &node.children {
        let path = join(prefix, name);
        if !child.children.is_empty() {
            out.insert(path.clone());
            collect_folders(child, &path, out);
        }
    }
}

/// Appends every visible row of `node` to `out`, depth-first and sorted.
fn walk(node: &Node, prefix: &str, depth: usize, expanded: &BTreeSet<String>, out: &mut Vec<Row>) {
    for (name, child) in &node.children {
        let path = join(prefix, name);
        let folder = !child.children.is_empty();
        let is_expanded = folder && expanded.contains(&path);
        out.push(Row {
            path: path.clone(),
            name: name.clone(),
            depth,
            folder,
            leaf: child.leaf,
            expanded: is_expanded,
        });
        if is_expanded {
            walk(child, &path, depth + 1, expanded, out);
        }
    }
}

/// Joins a parent path and a segment, without a leading separator at the root.
fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn an_empty_list_has_no_rows() {
        let tree = Tree::new(Vec::new());
        assert!(tree.rows("").is_empty());
        assert!(tree.paths().is_empty());
    }

    #[test]
    fn a_single_segment_is_a_root_leaf() {
        let tree = Tree::new(paths(&["sys"]));
        let rows = tree.rows("");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "sys");
        assert_eq!(rows[0].depth, 0);
        assert!(!rows[0].folder && rows[0].leaf);
    }

    #[test]
    fn sibling_prefixes_do_not_collide() {
        let mut tree = Tree::new(paths(&["sys/ui/mode", "sys/ui2"]));
        // Expand the root so the children are visible.
        tree.toggle("sys");
        tree.toggle("sys/ui");
        let rows = tree.rows("");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["sys", "ui", "mode", "ui2"]);
        // `sys/ui` is a folder with no value; `sys/ui2` is a leaf.
        assert!(rows[1].folder && !rows[1].leaf);
        assert!(rows[3].leaf && !rows[3].folder);
    }

    #[test]
    fn deep_paths_are_indented_by_depth() {
        let mut tree = Tree::new(paths(&["sys/a/b/c/d"]));
        for folder in ["sys", "sys/a", "sys/a/b", "sys/a/b/c"] {
            tree.toggle(folder);
        }
        let rows = tree.rows("");
        let depths: Vec<usize> = rows.iter().map(|row| row.depth).collect();
        assert_eq!(depths, [0, 1, 2, 3, 4]);
        assert_eq!(rows[4].path, "sys/a/b/c/d");
        assert!(rows[4].leaf);
        // Depth 4 indents eight spaces; a leaf adds a two-space marker.
        assert_eq!(rows[4].label(), "          d");
    }

    #[test]
    fn a_path_may_be_both_a_folder_and_a_leaf() {
        let mut tree = Tree::new(paths(&["sys/ui", "sys/ui/mode"]));
        tree.toggle("sys");
        let rows = tree.rows("");
        let ui = rows.iter().find(|row| row.path == "sys/ui").unwrap();
        assert!(ui.folder && ui.leaf);
    }

    #[test]
    fn expansion_survives_refresh_and_drops_for_removed_keys() {
        let mut tree = Tree::new(paths(&["sys/ui/mode", "sys/net/mtu"]));
        tree.toggle("sys");
        tree.toggle("sys/ui");
        assert!(tree.is_expanded("sys/ui"));
        // A refresh with the same paths keeps the state.
        tree.refresh(paths(&["sys/ui/mode", "sys/net/mtu"]));
        assert!(tree.is_expanded("sys/ui"));
        // Removing the only child under `sys/ui` drops its expansion.
        tree.refresh(paths(&["sys/net/mtu"]));
        assert!(!tree.is_expanded("sys/ui"));
        assert!(tree.is_expanded("sys"));
    }

    #[test]
    fn filter_keeps_ancestors_and_hides_non_matches() {
        let tree = Tree::new(paths(&["sys/ui/mode", "sys/ui2", "sys/net/mtu"]));
        let rows = tree.rows("mode");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        // `sys` and `ui` are kept as ancestors even though they do not match.
        assert_eq!(names, ["sys", "ui", "mode"]);
        assert!(rows.iter().all(|row| row.name != "ui2"));
        assert!(rows.iter().all(|row| row.name != "mtu"));
    }

    #[test]
    fn filter_auto_expands_so_a_match_is_visible() {
        // Nothing has been expanded, yet the match is on screen.
        let tree = Tree::new(paths(&["sys/ui/mode", "sys/net/mtu"]));
        let rows = tree.rows("mtu");
        assert_eq!(rows.last().unwrap().path, "sys/net/mtu");
    }

    #[test]
    fn toggle_round_trips() {
        let mut tree = Tree::new(paths(&["sys/ui/mode"]));
        assert!(!tree.is_expanded("sys"));
        tree.toggle("sys");
        assert!(tree.is_expanded("sys"));
        tree.toggle("sys");
        assert!(!tree.is_expanded("sys"));
    }
}
