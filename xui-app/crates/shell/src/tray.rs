//! The taskbar tray's model (docs/tray-plan.md section 7.1): which apps have
//! an item, what each shows, in which order, and where the cells sit.
//!
//! There is **one item per app**, keyed by the caller's kernel-stamped label
//! (the `init` app id for an unlabelled built-in): `Set` creates or replaces
//! the app's custom item, `Clear` (or a dead event channel) drops it. A
//! running *resident* app (the retained `session/<s>/apps/resident` topic)
//! always has an entry: with no custom item it shows its default item, and it
//! leaves the tray only when `init` reports it stopped.
//!
//! * [`item`]: the validated item and the checks a request goes through.
//! * [`icon`]: the picture fallback chain.
//! * [`layout`]: the cells on the bar, `TRAY_VISIBLE` and the overflow.

pub mod icon;
pub mod item;
pub mod layout;
pub mod menu;
pub mod policy;

use item::{Invalid, Item, Patch, Status};

/// Most items one session's tray holds (custom and default together).
pub const ITEMS_MAX: usize = 64;
/// Longest app key (a label or app id) the tray keeps.
pub const KEY_MAX: usize = 128;

/// One app's entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The app's label (or built-in app id).
    pub app: String,
    /// What the app set; `None` shows the default item.
    pub custom: Option<Item>,
    /// The resident instance's pid while `init` reports it running.
    pub resident: Option<u64>,
}

impl Entry {
    /// The item's status; a default item is `Active`.
    pub fn status(&self) -> Status {
        self.custom
            .as_ref()
            .map_or(Status::Active, |item| item.status)
    }

    /// The app's tooltip text (empty for a default item, which shows only
    /// the verified name).
    pub fn tooltip(&self) -> &str {
        self.custom
            .as_ref()
            .map_or("", |item| item.tooltip.as_str())
    }
}

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The tray already holds [`ITEMS_MAX`] items (`ENOSPC`).
    Full,
    /// `Update` before `Set` (`ENOENT`).
    NoItem,
    /// An unusable app key (`EINVAL`; never happens for a kernel label).
    Key,
    /// A field failed validation (`EINVAL`).
    Invalid(Invalid),
}

/// What a change did to the bar, for the `SHELL:TRAY:*` markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A new entry appeared.
    Added,
    /// An existing entry now shows something else.
    Replaced,
    /// A custom item went back to the resident default.
    Reverted,
    /// The entry left the tray.
    Removed,
    /// Nothing changed.
    Unchanged,
}

/// Every app's entry plus the user's order and hidden set.
#[derive(Clone, Debug, Default)]
pub struct Tray {
    /// In order of first appearance.
    entries: Vec<Entry>,
    /// `user/<uid>/tray/order`: these apps first, in this order.
    order: Vec<String>,
    /// `user/<uid>/tray/hidden/<app>`: kept in the overflow.
    hidden: Vec<String>,
}

impl Tray {
    pub fn new() -> Tray {
        Tray::default()
    }

    /// The entries in display order: the user's order first, then first
    /// appearance.
    pub fn entries(&self) -> Vec<&Entry> {
        let rank = |entry: &Entry| {
            self.order
                .iter()
                .position(|app| *app == entry.app)
                .unwrap_or(usize::MAX)
        };
        let mut out: Vec<&Entry> = self.entries.iter().collect();
        // Stable, so equal ranks keep first-appearance order.
        out.sort_by_key(|entry| rank(entry));
        out
    }

    /// How many entries the tray holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry of `app`.
    pub fn get(&self, app: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.app == app)
    }

    /// Whether the user hid `app`'s item.
    pub fn is_hidden(&self, app: &str) -> bool {
        self.hidden.iter().any(|hidden| hidden == app)
    }

    /// `Set`: show `item` for `app`, replacing what it showed.
    pub fn set(&mut self, app: &str, item: Item) -> Result<Change, Refused> {
        if let Some(entry) = self.entry_mut(app) {
            entry.custom = Some(item);
            return Ok(Change::Replaced);
        }
        self.insert(app, Some(item), None)?;
        Ok(Change::Added)
    }

    /// `Update`: apply `patch` to `app`'s custom item.
    pub fn update(&mut self, app: &str, patch: Patch) -> Result<Change, Refused> {
        let item = self
            .entry_mut(app)
            .and_then(|entry| entry.custom.as_mut())
            .ok_or(Refused::NoItem)?;
        item.apply(patch).map_err(Refused::Invalid)?;
        Ok(Change::Replaced)
    }

    /// `Clear`, or the app's event channel is gone: drop the custom item. A
    /// running resident app keeps its default item; any other app leaves.
    pub fn clear(&mut self, app: &str) -> Change {
        let Some(index) = self.index(app) else {
            return Change::Unchanged;
        };
        let entry = &mut self.entries[index];
        if entry.custom.take().is_none() {
            return Change::Unchanged;
        }
        if entry.resident.is_some() {
            Change::Reverted
        } else {
            self.entries.remove(index);
            Change::Removed
        }
    }

    /// The resident-apps topic: `running` is every resident app of the
    /// session and its pid. Apps new to it get an entry (default item);
    /// entries of apps no longer in it leave the tray, custom item and all,
    /// since the app has stopped. Returns each app whose entry was added or
    /// removed. Unusable keys, and apps past [`ITEMS_MAX`], are skipped.
    pub fn set_resident(&mut self, running: &[(String, u64)]) -> Vec<(String, Change)> {
        let mut changes = Vec::new();
        let mut kept = Vec::with_capacity(self.entries.len());
        for mut entry in core::mem::take(&mut self.entries) {
            let now = running
                .iter()
                .find(|(app, _)| *app == entry.app)
                .map(|(_, pid)| *pid);
            match (entry.resident, now) {
                (Some(_), None) => changes.push((entry.app, Change::Removed)),
                _ => {
                    entry.resident = now;
                    kept.push(entry);
                }
            }
        }
        self.entries = kept;
        for (app, pid) in running {
            if self.get(app).is_none() && self.insert(app, None, Some(*pid)).is_ok() {
                changes.push((app.clone(), Change::Added));
            }
        }
        changes
    }

    /// The user's order (`user/<uid>/tray/order`); unknown apps are kept for
    /// when they appear.
    pub fn set_order(&mut self, order: Vec<String>) {
        self.order = order;
        self.order.truncate(ITEMS_MAX);
    }

    /// The apps the user hid (`user/<uid>/tray/hidden/<app>`).
    pub fn set_hidden(&mut self, hidden: Vec<String>) {
        self.hidden = hidden;
        self.hidden.truncate(ITEMS_MAX);
    }

    fn insert(
        &mut self,
        app: &str,
        custom: Option<Item>,
        resident: Option<u64>,
    ) -> Result<(), Refused> {
        if app.is_empty() || app.len() > KEY_MAX || app.chars().any(char::is_control) {
            return Err(Refused::Key);
        }
        if self.entries.len() >= ITEMS_MAX {
            return Err(Refused::Full);
        }
        self.entries.push(Entry {
            app: app.to_owned(),
            custom,
            resident,
        });
        Ok(())
    }

    fn index(&self, app: &str) -> Option<usize> {
        self.entries.iter().position(|entry| entry.app == app)
    }

    fn entry_mut(&mut self, app: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.app == app)
    }
}

#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod menu_tests;
#[cfg(test)]
mod tests;
