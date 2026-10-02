//! The start menu: which rows it shows and where they sit.
//!
//! The rows come from `init`'s `ListApps` (issue #509). Apps the user hides
//! (`ListApps.hidden`, the caller's `user/<uid>/menu/hidden/<sn>` over the
//! machine's `sys/menu/hidden/<sn>`) are left out of both row groups before
//! [`Menu::build`] sees them ([`visible`], [`installed_entries`]); they still
//! launch and open files.
//!
//! Rows run top-down: the desktop apps (installed ones and the built-in
//! desktop programs, everything `ListApps` gives a category) no `sys/ui/menu`
//! entry pins, grouped by menu category under a header row each ([`groups`]),
//! then the configured `sys/ui/menu` entries, then the two power rows
//! ([`power`]). The installed section holds at most
//! [`groups::MAX_PER_CATEGORY`] apps per category and scrolls with the wheel
//! ([`Menu::scroll_by`]) when it is taller than the room above the configured
//! rows. A configured entry the image does not ship stays in the list,
//! greyed and disabled, so configured row `j` of `m` always has the same
//! centre however the image was built and whatever is installed (the
//! screenshot sessions click rows by coordinate): its centre is
//! `y = H - 48 - (m + 1 - j) * 24`, and the power rows' centres are `H - 72`
//! ("Restart...") and `H - 48` ("Shut down..."). With the default menu
//! (13 entries) on a 720-pixel screen, Terminal is at `y = 336`, Settings at
//! `y = 504` and Devices at `y = 624`, all at `x = 134`. Hiding a configured
//! app removes its row, which moves the rows above it down by one.
//!
//! The panel is [`WIDTH`] wide, sits at `x = 0` with its bottom edge on the
//! taskbar's top edge, and has a [`BANNER_W`]-wide vertical banner on its
//! left. Geometry here is panel-local.

use deskmenu::Entry;

use crate::taskbar::BAR_H;

pub mod groups;
mod power;

use crate::Rect;
pub use groups::{InstalledApp, MAX_PER_CATEGORY};
pub use power::{Action, Choice, Power, POWER_ROWS};

/// Panel width.
pub const WIDTH: i32 = 240;
/// The vertical "LazyOS" banner on the left.
pub const BANNER_W: i32 = 28;
/// Row height.
pub const ROW_H: i32 = 24;
/// Padding above the first and below the last row.
pub const PAD: i32 = 4;

/// One start-menu row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The `init` registry app it launches.
    pub app: String,
    pub label: String,
    /// Whether the image ships the app; a disabled row is drawn greyed and
    /// does nothing when clicked.
    pub enabled: bool,
    /// What choosing it does ([`Action::Launch`] for an app row; `app` is
    /// empty for the power rows).
    pub action: Action,
}

/// The rows, installed apps first, power rows last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    /// The rows on screen: `section[scroll..scroll + window]`, the
    /// configured rows, the power rows.
    rows: Vec<Row>,
    /// The whole installed section, headers included.
    section: Vec<Row>,
    /// The first section row on screen.
    scroll: usize,
    /// How many section rows are on screen.
    window: usize,
}

/// Where the installed section's visible part sits in it, for a scroll bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scroll {
    /// The first visible section row.
    pub first: usize,
    /// Visible section rows (the top `shown` rows of the panel).
    pub shown: usize,
    /// All section rows.
    pub total: usize,
}

/// The app list `init` reported, for deciding which rows are enabled.
#[derive(Clone, Copy)]
pub enum Shipped<'a> {
    /// `init` answered: exactly these ids are launchable.
    Known(&'a [String]),
    /// `init` could not be asked: treat every row as enabled (a launch of an
    /// unshipped app is refused by `init` anyway).
    Unknown,
}

impl Shipped<'_> {
    fn has(&self, app: &str) -> bool {
        match self {
            // A saved short id (`editor`) is shipped when its core app is.
            Shipped::Known(ids) => ids.iter().any(|id| deskmenu::same_app(id, app)),
            Shipped::Unknown => true,
        }
    }
}

impl Menu {
    /// Build the menu for a screen `screen_h` tall: the `installed` apps no
    /// configured entry names, grouped by category ([`groups::rows`]), then
    /// `configured`, then the power rows. When the rows do not fit above the
    /// taskbar, the installed section shrinks first (and scrolls), so the
    /// configured rows keep their positions; the power rows always stay.
    pub fn build(
        installed: &[InstalledApp],
        configured: &[Entry],
        shipped: Shipped<'_>,
        screen_h: i32,
    ) -> Menu {
        let fit = usize::try_from((screen_h - BAR_H - PAD * 2) / ROW_H)
            .unwrap_or(0)
            .saturating_sub(POWER_ROWS);
        let configured = &configured[..configured.len().min(fit)];
        let room = fit - configured.len();
        let unpinned: Vec<InstalledApp> = installed
            .iter()
            .filter(|app| {
                !configured
                    .iter()
                    .any(|c| deskmenu::same_app(&c.app, &app.entry.app))
            })
            .cloned()
            .collect();
        let section = groups::rows(&unpinned);
        let window = section.len().min(room);
        let mut rows: Vec<Row> = section[..window].to_vec();
        rows.extend(configured.iter().map(|entry| Row {
            app: entry.app.clone(),
            label: entry.label.clone(),
            enabled: shipped.has(&entry.app),
            action: Action::Launch,
        }));
        rows.extend(power::ask_rows());
        Menu {
            rows,
            section,
            scroll: 0,
            window,
        }
    }

    /// Scroll the installed section by `lines` rows (negative: up), within
    /// its bounds. Returns whether anything moved.
    pub fn scroll_by(&mut self, lines: i64) -> bool {
        let last = i64::try_from(self.section.len() - self.window).unwrap_or(0);
        let now = i64::try_from(self.scroll).unwrap_or(0);
        let target = usize::try_from(now.saturating_add(lines).clamp(0, last)).unwrap_or(0);
        if target == self.scroll {
            return false;
        }
        self.scroll = target;
        let shown = &self.section[target..target + self.window];
        self.rows.splice(..self.window, shown.iter().cloned());
        true
    }

    /// The installed section's scroll position, when it does not all fit.
    pub fn scroll(&self) -> Option<Scroll> {
        (self.window < self.section.len()).then_some(Scroll {
            first: self.scroll,
            shown: self.window,
            total: self.section.len(),
        })
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The panel height for the current rows.
    pub fn height(&self) -> i32 {
        self.rows.len() as i32 * ROW_H + PAD * 2
    }

    /// The panel's screen origin on a screen `screen_h` tall: bottom edge on
    /// the taskbar's top edge.
    pub fn origin(&self, screen_h: i32) -> (i32, i32) {
        (0, (screen_h - BAR_H - self.height()).max(0))
    }

    /// Row `index`'s panel-local rectangle.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        (index < self.rows.len()).then(|| {
            Rect::new(
                BANNER_W,
                PAD + index as i32 * ROW_H,
                WIDTH - BANNER_W,
                ROW_H,
            )
        })
    }

    /// The row under panel-local `(x, y)`, enabled or not.
    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if !(BANNER_W..WIDTH).contains(&x) || y < PAD {
            return None;
        }
        let index = usize::try_from((y - PAD) / ROW_H).ok()?;
        (index < self.rows.len()).then_some(index)
    }

    /// The row launching `app`.
    pub fn find(&self, app: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.action == Action::Launch && row.app == app)
    }

    /// The row with `action`.
    pub fn find_action(&self, action: Action) -> Option<usize> {
        self.rows.iter().position(|row| row.action == action)
    }
}

/// `entries` without the apps `hidden` says the menu leaves out, order kept.
pub fn visible(mut entries: Vec<Entry>, hidden: impl Fn(&str) -> bool) -> Vec<Entry> {
    entries.retain(|entry| !hidden(&entry.app));
    entries
}

/// One `ListApps` row as the menu reads it.
#[derive(Clone, Copy, Debug)]
pub struct Listed<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// Installed by the package manager (`id` is a `system_name`).
    pub installed: bool,
    /// The menu group; empty for a console program, which the menu omits.
    pub category: &'a str,
    /// Left out of the caller's menu.
    pub hidden: bool,
}

/// Whether `app` (a configured id, possibly a pre-F5 short id) is one of the
/// `listed` apps `ListApps` marked hidden for the caller.
pub fn listed_hidden(listed: &[Listed<'_>], app: &str) -> bool {
    listed
        .iter()
        .any(|row| row.hidden && deskmenu::same_app(row.id, app))
}

/// The menu's app rows from `init`'s registry rows: the desktop apps (every
/// installed one, and the built-ins with a category such as the Terminal)
/// that are not hidden, labelled with their name (cleaned, falling back to
/// the id). Console programs (no category) stay out.
pub fn installed_entries<'a>(apps: impl IntoIterator<Item = Listed<'a>>) -> Vec<InstalledApp> {
    apps.into_iter()
        .filter(|row| (row.installed || !row.category.is_empty()) && !row.hidden)
        .filter_map(|row| {
            installed_entry(row.id, row.name).map(|entry| InstalledApp {
                entry,
                category: String::from(row.category),
            })
        })
        .collect()
}

/// One installed-app row. Its id is the package's reverse-DNS `system_name`
/// (`org.lazy.counter`), which may be longer than a configured entry's id
/// (`deskmenu::MAX_APP`), so it is checked against the system-name rule.
fn installed_entry(id: &str, name: &str) -> Option<Entry> {
    if !deskmenu::valid_system_name(id) {
        return None;
    }
    let label = deskmenu::clean_label(name);
    Some(Entry {
        app: String::from(id),
        label: if label.is_empty() {
            String::from(id)
        } else {
            label
        },
    })
}

#[cfg(test)]
mod tests;
