//! The installed-app section of the start menu, grouped by the package's
//! menu `category` (issue #509).
//!
//! Each category that has an app gets a header row, then at most
//! [`MAX_PER_CATEGORY`] of its apps sorted by label. The categories come in
//! `lazypkg::Category::ALL` order; a category this build does not know is
//! filed under the manifest default, `accessories`, so a newer package never
//! disappears from the menu.

use deskmenu::Entry;

use super::{Action, Row};

/// Most apps one category lists. The section scrolls when the categories
/// together do not fit above the configured rows.
pub const MAX_PER_CATEGORY: usize = 16;

/// The menu categories in order, with their header labels. The spellings are
/// `lazypkg::Category::as_str`'s (the shell does not link the package
/// library).
pub const CATEGORIES: [(&str, &str); 8] = [
    ("accessories", "Accessories"),
    ("development", "Development"),
    ("games", "Games"),
    ("graphics", "Graphics"),
    ("internet", "Internet"),
    ("office", "Office"),
    ("system", "System"),
    ("utilities", "Utilities"),
];

/// The category an unknown spelling falls back to (the manifest default).
const DEFAULT_CATEGORY: &str = "accessories";

/// One installed app as the menu sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    pub entry: Entry,
    /// The manifest `category` spelling, as `ListApps` reported it.
    pub category: String,
}

impl InstalledApp {
    /// The known category this app is filed under.
    fn group(&self) -> &str {
        CATEGORIES
            .iter()
            .map(|(id, _)| *id)
            .find(|id| *id == self.category)
            .unwrap_or(DEFAULT_CATEGORY)
    }
}

/// A category header: not an app, never enabled, drawn as a title.
fn header(label: &str) -> Row {
    Row {
        app: String::new(),
        label: String::from(label),
        enabled: false,
        action: Action::Header,
    }
}

/// The section's rows: per category, its header and up to
/// [`MAX_PER_CATEGORY`] of `apps` sorted by label (ties by id, so the order
/// is stable whatever order `init` listed them in).
pub fn rows(apps: &[InstalledApp]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (id, label) in CATEGORIES {
        let mut members: Vec<&InstalledApp> = apps.iter().filter(|app| app.group() == id).collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by(|a, b| {
            let key = |app: &InstalledApp| app.entry.label.to_ascii_lowercase();
            key(a)
                .cmp(&key(b))
                .then_with(|| a.entry.app.cmp(&b.entry.app))
        });
        rows.push(header(label));
        rows.extend(members.into_iter().take(MAX_PER_CATEGORY).map(|app| Row {
            app: app.entry.app.clone(),
            label: app.entry.label.clone(),
            enabled: true,
            action: Action::Launch,
        }));
    }
    rows
}
