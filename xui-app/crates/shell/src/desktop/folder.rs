//! The desktop as a folder: what `$HOME/Desktop` holds, as desktop icons.
//!
//! Every visible entry is an icon: a shortcut file ([`crate::shortcut`])
//! shows its label and opens its target, a folder opens in Files and any
//! other file opens with its app (`mimed`). Names starting with a dot are
//! hidden, like everywhere else. The folder is created and seeded once, the
//! first time the shell finds it missing, with one shortcut per
//! `sys/ui/desktop` launcher ([`seed`]); after that it is the user's: what
//! they delete stays deleted.
//!
//! Icons keep a stable order through [`ORDER_FILE`], a hidden file listing
//! file names one per line (the seed writes it so the launchers keep their
//! places). Names it does not list follow it: folders first, then
//! everything else, each by label regardless of case.

use deskmenu::Entry;

use crate::shortcut::{self, Target};

/// The hidden file that keeps the icons' order.
pub const ORDER_FILE: &str = ".order";
/// Most names the order file is read for (a longer one is cut).
const MAX_ORDER: usize = 1024;

/// What an icon opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A shortcut to an `init` registry app.
    App(String),
    /// A shortcut to a path (absolute).
    Link(String),
    /// A folder in the desktop folder.
    Folder,
    /// Any other file in it.
    File,
}

/// One desktop icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The entry's file name in the desktop folder (empty for a launcher
    /// shown without a folder).
    pub name: String,
    pub label: String,
    pub kind: Kind,
}

impl Item {
    /// The app a shortcut launches.
    pub fn app(&self) -> Option<&str> {
        match &self.kind {
            Kind::App(app) => Some(app),
            _ => None,
        }
    }

    /// An icon for a launcher when there is no desktop folder (no `$HOME`):
    /// the shell then shows `sys/ui/desktop` directly, as before.
    pub fn launcher(entry: &Entry) -> Item {
        Item {
            name: String::new(),
            label: entry.label.clone(),
            kind: Kind::App(entry.app.clone()),
        }
    }
}

/// One directory entry as the shell read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// A `.lnk` file's text, when it was small enough to read.
    pub text: Option<String>,
}

/// The icons for a folder listing, ordered by `order` (the order file's
/// text, if any).
pub fn items(entries: &[DirEntry], order: Option<&str>) -> Vec<Item> {
    let mut items: Vec<Item> = entries
        .iter()
        .filter(|entry| !entry.name.starts_with('.') && !entry.name.is_empty())
        .map(item)
        .collect();
    let listed: Vec<&str> = order
        .map(|text| text.lines().map(str::trim).take(MAX_ORDER).collect())
        .unwrap_or_default();
    let rank = |item: &Item| listed.iter().position(|name| *name == item.name);
    items.sort_by(|a, b| match (rank(a), rank(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => core::cmp::Ordering::Less,
        (None, Some(_)) => core::cmp::Ordering::Greater,
        (None, None) => (a.kind != Kind::Folder)
            .cmp(&(b.kind != Kind::Folder))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name)),
    });
    items
}

/// One entry's icon: a parsed shortcut, a folder or a plain file.
fn item(entry: &DirEntry) -> Item {
    let name = entry.name.clone();
    if entry.is_dir {
        return Item {
            label: name.clone(),
            name,
            kind: Kind::Folder,
        };
    }
    let shortcut = shortcut::label_of(&name).zip(entry.text.as_deref().and_then(shortcut::parse));
    match shortcut {
        Some((label, target)) => Item {
            label: label.to_owned(),
            kind: match target {
                Target::App(app) => Kind::App(app),
                Target::Path(path) => Kind::Link(path),
            },
            name,
        },
        None => Item {
            label: name.clone(),
            name,
            kind: Kind::File,
        },
    }
}

/// The files a fresh desktop folder starts with: one shortcut per launcher
/// (`(file name, text)`, skipping a label that leaves no usable or a
/// duplicate name), then the order file listing them.
pub fn seed(launchers: &[Entry]) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in launchers {
        let Some(name) = shortcut::file_name(&entry.label) else {
            continue;
        };
        if files.iter().any(|(taken, _)| taken.eq_ignore_ascii_case(&name)) {
            continue;
        }
        files.push((name, shortcut::encode(&Target::App(entry.app.clone()))));
    }
    let order: String = files.iter().map(|(name, _)| format!("{name}\n")).collect();
    files.push((String::from(ORDER_FILE), order));
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, text: Option<&str>) -> DirEntry {
        DirEntry {
            name: name.into(),
            is_dir: false,
            text: text.map(Into::into),
        }
    }

    fn dir(name: &str) -> DirEntry {
        DirEntry {
            name: name.into(),
            is_dir: true,
            text: None,
        }
    }

    fn labels(items: &[Item]) -> Vec<&str> {
        items.iter().map(|item| item.label.as_str()).collect()
    }

    #[test]
    fn the_seed_is_one_shortcut_per_launcher_in_order() {
        let files = seed(&super::super::defaults());
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Files.lnk",
                "Terminal.lnk",
                "Editor.lnk",
                "Settings.lnk",
                "System Monitor.lnk",
                "Paint.lnk",
                ORDER_FILE
            ]
        );
        let entries: Vec<DirEntry> = files
            .iter()
            .map(|(name, text)| file(name, Some(text)))
            .collect();
        let items = items(&entries, Some(&files.last().unwrap().1));
        assert_eq!(
            labels(&items),
            ["Files", "Terminal", "Editor", "Settings", "System Monitor", "Paint"],
            "the launchers keep their places; the order file is hidden"
        );
        assert_eq!(items[0].app(), Some("os.lazy.files"));
        assert_eq!(items[1].kind, Kind::App("terminal".into()));
    }

    #[test]
    fn duplicate_or_empty_labels_are_seeded_once() {
        let launchers = [
            Entry::new("os.lazy.files", "Files").unwrap(),
            Entry::new("os.lazy.editor", "files").unwrap(),
        ];
        assert_eq!(seed(&launchers).len(), 2, "one shortcut and the order");
    }

    #[test]
    fn unlisted_entries_follow_folders_first_by_label() {
        let entries = [
            file("zeta.txt", None),
            dir("Projects"),
            file("Files.lnk", Some("App=os.lazy.files\n")),
            file("alpha.png", None),
            dir("archive"),
            file(".secret", None),
            file("broken.lnk", Some("App=no such id\n")),
            file("Docs.lnk", Some("Path=/docs/os\n")),
        ];
        let items = items(&entries, Some("Files.lnk\nmissing.lnk\n"));
        assert_eq!(
            labels(&items),
            ["Files", "archive", "Projects", "alpha.png", "broken.lnk", "Docs", "zeta.txt"]
        );
        let kinds: Vec<&Kind> = items.iter().map(|item| &item.kind).collect();
        assert_eq!(kinds[1], &Kind::Folder);
        assert_eq!(kinds[4], &Kind::File, "a shortcut that does not parse");
        assert_eq!(kinds[5], &Kind::Link("/docs/os".into()));
    }

    #[test]
    fn without_an_order_file_everything_sorts() {
        let entries = [file("b.lnk", Some("App=terminal\n")), file("A.txt", None)];
        assert_eq!(labels(&items(&entries, None)), ["A.txt", "b"]);
    }
}
