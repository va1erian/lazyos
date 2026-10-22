//! The list the Bookmarks menu shows: a plan made from the store (pure, tested), and the popup
//! built from the plan.
//!
//! The Bookmarks bar's entries come first, then "Other bookmarks" and "Unsorted" as submenus when
//! they hold anything. The list is capped: a person with a thousand bookmarks does not get a menu
//! a thousand rows tall, and every row past the cap is counted in one "... more" row.

use xui_core::app::Ui;
use xui_core::icon::Lucide;
use xui_core::widget::{Menu, MenuId, MenuScope};

use crate::app::Msg;
use crate::bookmarks::BookmarkMsg;
use crate::places::{model::root_title, Bookmarks, Id, Kind, BAR, OTHER, UNSORTED};
use crate::sanitize;

/// The most rows (links, folders and the "more" rows) in the whole list.
pub const MAX_ROWS: usize = 40;
/// How many levels of folders open as submenus (the popup pool is one window per level).
pub const MAX_LEVELS: usize = 3;
/// The longest row text, in characters.
const MAX_LABEL: usize = 48;

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Opens bookmark `id`.
    Link {
        id: Id,
        label: String,
    },
    Folder {
        label: String,
        rows: Vec<Row>,
    },
    Separator,
    /// "N more", not a command.
    More(usize),
    /// Nothing is bookmarked, not a command.
    Empty,
}

/// What to show for the store as it is now.
pub fn plan(set: &Bookmarks) -> Vec<Row> {
    let mut budget = MAX_ROWS;
    let mut rows = level(set, BAR, 1, &mut budget);
    let mut roots = Vec::new();
    for root in [OTHER, UNSORTED] {
        if set.children(root).is_empty() {
            continue;
        }
        let label = root_title(root).unwrap_or_default().to_owned();
        if budget == 0 {
            roots.push(Row::More(set.children(root).len()));
            continue;
        }
        budget -= 1;
        let inner = level(set, root, 2, &mut budget);
        roots.push(Row::Folder { label, rows: inner });
    }
    if !roots.is_empty() {
        if !rows.is_empty() {
            rows.push(Row::Separator);
        }
        rows.extend(roots);
    }
    if rows.is_empty() {
        rows.push(Row::Empty);
    }
    rows
}

/// The rows of `parent` at nesting `depth`, taking from the shared `budget`.
fn level(set: &Bookmarks, parent: Id, depth: usize, budget: &mut usize) -> Vec<Row> {
    let children = set.children(parent);
    let mut rows = Vec::new();
    for (shown, entry) in children.iter().enumerate() {
        if *budget == 0 {
            rows.push(Row::More(children.len() - shown));
            break;
        }
        match entry.kind {
            Kind::Separator => rows.push(Row::Separator),
            Kind::Link => {
                *budget -= 1;
                rows.push(Row::Link {
                    id: entry.id,
                    label: label_of(&entry.title, &entry.url),
                });
            }
            Kind::Folder => {
                *budget -= 1;
                // Past the last level a folder shows as a row with nothing under it.
                let inner = if depth < MAX_LEVELS {
                    level(set, entry.id, depth + 1, budget)
                } else {
                    Vec::new()
                };
                rows.push(Row::Folder {
                    label: label_of(&entry.title, ""),
                    rows: inner,
                });
            }
        }
    }
    rows
}

/// The text of a row: the title, else the address, kept short.
fn label_of(title: &str, url: &str) -> String {
    let text = if title.is_empty() { url } else { title };
    let text = sanitize::line(text, MAX_LABEL + 1);
    if text.chars().count() > MAX_LABEL {
        format!("{}...", sanitize::clip_chars(&text, MAX_LABEL - 3))
    } else if text.is_empty() {
        String::from("(untitled)")
    } else {
        text
    }
}

/// A row's text as a menu label: `&` would mark a mnemonic, so it is doubled.
fn escaped(label: &str) -> String {
    label.replace('&', "&&")
}

/// The first id of the list's link rows; the index of a row is its id minus this.
const LINK_BASE: usize = 1_000;
/// Where submenu and inert rows' ids start (never raised as a choice).
const OTHER_BASE: usize = 1_000_000;

/// The popup for `rows` and the bookmark each of its link rows opens, by index.
pub fn build(ui: &Ui<Msg>, rows: &[Row]) -> (Menu<Msg>, Vec<Id>) {
    let mut targets = Vec::new();
    let mut inert = Vec::new();
    let mut next_other = OTHER_BASE;
    let menu = Menu::context(ui)
        .build(|m| fill(m, rows, &mut targets, &mut inert, &mut next_other))
        .on_select(|id: MenuId| {
            // Ids are opaque; the link rows are the ones in the first range.
            link_index(id).map(|n| Msg::Bookmarks(BookmarkMsg::Pick(n)))
        });
    for id in inert {
        menu.set_enabled(id, false);
    }
    (menu, targets)
}

fn link_index(id: MenuId) -> Option<usize> {
    // `MenuId` has no accessor; the ids are compared by value.
    (0..MAX_ROWS).find(|n| MenuId::new(LINK_BASE + n) == id)
}

fn fill(
    m: &mut MenuScope<'_>,
    rows: &[Row],
    targets: &mut Vec<Id>,
    inert: &mut Vec<MenuId>,
    next_other: &mut usize,
) {
    for row in rows {
        match row {
            Row::Link { id, label } => {
                let at = targets.len();
                targets.push(*id);
                m.item(MenuId::new(LINK_BASE + at), &escaped(label))
                    .icon(Lucide::Link);
            }
            Row::Folder { label, rows } => {
                let id = MenuId::new(*next_other);
                *next_other += 1;
                if rows.is_empty() {
                    // A submenu with nothing in it would not open: show it as an inert row.
                    m.item(id, &escaped(label)).icon(Lucide::Folder);
                    inert.push(id);
                } else {
                    m.submenu(id, &escaped(label), |sub| {
                        fill(sub, rows, targets, inert, next_other)
                    });
                }
            }
            Row::Separator => {
                m.separator();
            }
            Row::More(n) => {
                let id = MenuId::new(*next_other);
                *next_other += 1;
                m.item(id, &format!("... {n} more"));
                inert.push(id);
            }
            Row::Empty => {
                let id = MenuId::new(*next_other);
                *next_other += 1;
                m.item(id, "No bookmarks yet");
                inert.push(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_with(n: usize) -> Bookmarks {
        let mut set = Bookmarks::in_memory();
        for i in 0..n {
            set.add_link(BAR, &format!("t{i}"), &format!("http://h{i}.test/"), 1)
                .unwrap();
        }
        set
    }

    fn count(rows: &[Row]) -> usize {
        rows.iter()
            .map(|r| match r {
                Row::Link { .. } => 1,
                Row::Folder { rows, .. } => 1 + count(rows),
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn an_empty_store_shows_one_inert_row() {
        assert_eq!(plan(&Bookmarks::in_memory()), [Row::Empty]);
    }

    #[test]
    fn the_bar_comes_first_then_the_other_roots_as_submenus() {
        let mut set = set_with(2);
        set.add_link(UNSORTED, "u", "http://u.test/", 1).unwrap();
        let f = set.add_folder(BAR, "News & Co", 1).unwrap();
        set.add_link(f, "inside", "http://i.test/", 1).unwrap();
        let rows = plan(&set);
        assert!(matches!(&rows[0], Row::Link { label, .. } if label == "t0"));
        assert!(
            matches!(&rows[2], Row::Folder { label, rows } if label == "News & Co" && rows.len() == 1)
        );
        assert_eq!(rows[3], Row::Separator);
        assert!(matches!(&rows[4], Row::Folder { label, .. } if label == "Unsorted"));
        // An empty "Other bookmarks" is left out.
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn the_whole_list_is_capped_and_says_how_many_are_left() {
        let rows = plan(&set_with(MAX_ROWS + 25));
        assert_eq!(count(&rows), MAX_ROWS);
        assert_eq!(rows.last(), Some(&Row::More(25)));
    }

    #[test]
    fn the_cap_is_shared_by_the_roots_and_folders() {
        let mut set = set_with(MAX_ROWS - 1);
        for i in 0..10 {
            set.add_link(UNSORTED, "u", &format!("http://u{i}.test/"), 1)
                .unwrap();
        }
        let rows = plan(&set);
        assert!(count(&rows) <= MAX_ROWS);
        assert!(rows
            .iter()
            .any(|r| matches!(r, Row::Folder { .. } | Row::More(_))));
    }

    #[test]
    fn folders_open_only_a_few_levels() {
        let mut set = Bookmarks::in_memory();
        let mut parent = BAR;
        for _ in 0..8 {
            parent = set.add_folder(parent, "d", 1).unwrap();
        }
        set.add_link(parent, "deep", "http://deep.test/", 1)
            .unwrap();
        fn depth(rows: &[Row]) -> usize {
            rows.iter()
                .map(|r| match r {
                    Row::Folder { rows, .. } => 1 + depth(rows),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        assert_eq!(depth(&plan(&set)), MAX_LEVELS);
    }

    #[test]
    fn labels_are_one_short_line_and_never_empty() {
        assert_eq!(label_of("", "http://a.test/x"), "http://a.test/x");
        assert_eq!(label_of("", ""), "(untitled)");
        let long = label_of(&"w".repeat(200), "");
        assert_eq!(long.chars().count(), MAX_LABEL);
        assert!(long.ends_with("..."));
        assert_eq!(label_of("a\u{202E}b\nc", ""), "abc");
        assert_eq!(escaped("R&D"), "R&&D");
    }
}
