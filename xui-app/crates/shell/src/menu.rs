//! The start menu: which rows it shows and where they sit.
//!
//! Apps the user hides (`deskmenu::hidden`, issue #509) are left out of both
//! row groups before [`Menu::build`] sees them ([`visible`]); they still
//! launch and open files.
//!
//! Rows run top-down: the apps the package manager installed (`init`'s
//! `ListApps` rows with `installed` set) first, then the configured
//! `sys/ui/menu` entries, then the two power rows ([`power`]). A configured
//! entry the image does not ship stays in the list, greyed and disabled, so
//! configured row `j` of `m` always has the same centre however the image was
//! built (the screenshot sessions click rows by coordinate): its centre is
//! `y = H - 48 - (m + 1 - j) * 24`, and the power rows' centres are `H - 72`
//! ("Restart...") and `H - 48` ("Shut down...").
//!
//! The panel is [`WIDTH`] wide, sits at `x = 0` with its bottom edge on the
//! taskbar's top edge, and has a [`BANNER_W`]-wide vertical banner on its
//! left. Geometry here is panel-local.

use deskmenu::hidden::Hidden;
use deskmenu::Entry;

use crate::taskbar::BAR_H;

mod power;

use crate::Rect;
pub use power::{Action, Choice, Power, POWER_ROWS};

/// Panel width.
pub const WIDTH: i32 = 240;
/// The vertical "LazyOS" banner on the left.
pub const BANNER_W: i32 = 28;
/// Row height.
pub const ROW_H: i32 = 24;
/// Padding above the first and below the last row.
pub const PAD: i32 = 4;
/// Most installed apps listed (the same cap the old compositor menu used).
pub const MAX_INSTALLED: usize = 16;

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
    rows: Vec<Row>,
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
    /// Build the menu for a screen `screen_h` tall: `installed` (capped at
    /// [`MAX_INSTALLED`], minus any app also configured), `configured`, then
    /// the power rows. When the rows do not fit above the taskbar, installed
    /// rows are dropped first, so the configured rows keep their positions;
    /// the power rows always stay.
    pub fn build(
        installed: &[Entry],
        configured: &[Entry],
        shipped: Shipped<'_>,
        screen_h: i32,
    ) -> Menu {
        let fit = usize::try_from((screen_h - BAR_H - PAD * 2) / ROW_H)
            .unwrap_or(0)
            .saturating_sub(POWER_ROWS);
        let configured = &configured[..configured.len().min(fit)];
        let room = fit - configured.len();
        let mut rows: Vec<Row> = installed
            .iter()
            .filter(|entry| !configured.iter().any(|c| deskmenu::same_app(&c.app, &entry.app)))
            .take(MAX_INSTALLED.min(room))
            .map(|entry| Row {
                app: entry.app.clone(),
                label: entry.label.clone(),
                enabled: true,
                action: Action::Launch,
            })
            .collect();
        rows.extend(configured.iter().map(|entry| Row {
            app: entry.app.clone(),
            label: entry.label.clone(),
            enabled: shipped.has(&entry.app),
            action: Action::Launch,
        }));
        rows.extend(power::ask_rows());
        Menu { rows }
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

/// `entries` without the apps `hidden` leaves out of the menu, order kept.
pub fn visible(mut entries: Vec<Entry>, hidden: &Hidden) -> Vec<Entry> {
    entries.retain(|entry| !hidden.hides(&entry.app));
    entries
}

/// The installed-app rows from `init`'s registry rows `(id, name, installed)`:
/// installed ones only, labelled with their manifest name (cleaned, falling
/// back to the id).
pub fn installed_entries<'a>(
    apps: impl IntoIterator<Item = (&'a str, &'a str, bool)>,
) -> Vec<Entry> {
    apps.into_iter()
        .filter(|(_, _, installed)| *installed)
        .filter_map(|(id, name, _)| installed_entry(id, name))
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
