//! A tray item's menu as the shell shows it (docs/tray-plan.md section 7.2):
//! the app's declarative rows, then a separator and the shell's own
//! **Quit <App>** row, which the app cannot remove or relabel because it is
//! not part of the app's rows at all.
//!
//! A panel shows one level: the top level ([`TrayMenu::top`]) or the rows of
//! one `Submenu` row ([`TrayMenu::submenu`]). Geometry is panel-local design
//! pixels, like the start menu's.

use super::item::{MenuKind, MenuRow};
use crate::Rect;

/// The panel's width.
pub const WIDTH: i32 = 220;
/// A row's height, and a separator's.
pub const ROW_H: i32 = 24;
pub const SEPARATOR_H: i32 = 9;
/// Padding above the first and below the last row.
pub const PAD: i32 = 4;

/// What a shown row is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An app row: picking it sends `MenuItem(id, checked)`.
    Item {
        id: u32,
        kind: MenuKind,
        checked: bool,
    },
    /// Opens the rows whose `parent` is `id`.
    Submenu {
        id: u32,
    },
    Separator,
    /// The shell's own row: stop the app through `init`.
    Quit,
}

/// One shown row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub label: String,
    pub enabled: bool,
    pub kind: Kind,
}

/// What picking a row asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Send `MenuItem(id, checked)`: a check row's new state, `true` for a
    /// radio row, the row's own state otherwise.
    Item { id: u32, checked: bool },
    /// Open the submenu of row `id`.
    Submenu { id: u32 },
    /// Stop the app.
    Quit,
}

/// One panel's rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayMenu {
    rows: Vec<Shown>,
}

impl TrayMenu {
    /// The top level of an item with `rows`, ending with the Quit row named
    /// after the app's verified registry `name`.
    pub fn top(rows: &[MenuRow], name: &str) -> TrayMenu {
        let mut shown = level(rows, 0);
        if !shown.is_empty() {
            shown.push(separator());
        }
        shown.push(Shown {
            label: quit_label(name),
            enabled: true,
            kind: Kind::Quit,
        });
        TrayMenu { rows: shown }
    }

    /// The rows of the `Submenu` row `id` (no Quit row: it is on the top
    /// level).
    pub fn submenu(rows: &[MenuRow], id: u32) -> TrayMenu {
        TrayMenu {
            rows: level(rows, id),
        }
    }

    pub fn rows(&self) -> &[Shown] {
        &self.rows
    }

    /// The panel's height.
    pub fn height(&self) -> i32 {
        2 * PAD + self.rows.iter().map(row_height).sum::<i32>()
    }

    /// Row `index`'s rectangle.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        let row = self.rows.get(index)?;
        let top = PAD + self.rows[..index].iter().map(row_height).sum::<i32>();
        Some(Rect::new(0, top, WIDTH, row_height(row)))
    }

    /// The row under `(x, y)`.
    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.rows.len()).find(|&index| self.row_rect(index).is_some_and(|r| r.contains(x, y)))
    }

    /// What picking row `index` does; `None` for a separator or a disabled
    /// row.
    pub fn pick(&self, index: usize) -> Option<Pick> {
        let row = self.rows.get(index)?;
        if !row.enabled {
            return None;
        }
        match row.kind {
            Kind::Item { id, kind, checked } => Some(Pick::Item {
                id,
                checked: next_checked(kind, checked),
            }),
            Kind::Submenu { id } => Some(Pick::Submenu { id }),
            Kind::Separator => None,
            Kind::Quit => Some(Pick::Quit),
        }
    }

    /// The panel's top-left corner for a panel `height` tall opened from the
    /// tray cell `cell` (screen design pixels) on a `screen` sized display
    /// whose bar starts at `bar_y`: above the bar, right-aligned with the
    /// cell's right edge, kept on screen.
    pub fn origin(cell: Rect, height: i32, screen: (i32, i32), bar_y: i32) -> (i32, i32) {
        let x = (cell.x + cell.w - WIDTH).clamp(0, (screen.0 - WIDTH).max(0));
        let y = (bar_y - height).max(0);
        (x, y)
    }
}

/// The row a `DefaultItem` click runs, as a pick.
pub fn default_pick(rows: &[MenuRow]) -> Option<Pick> {
    let row = rows.iter().find(|row| row.default && row.enabled)?;
    Some(Pick::Item {
        id: row.id,
        checked: next_checked(row.kind, row.checked),
    })
}

/// The label of the shell's Quit row.
pub fn quit_label(name: &str) -> String {
    format!("Quit {name}")
}

/// A check row flips; a radio row turns on; anything else reports its
/// state unchanged.
fn next_checked(kind: MenuKind, checked: bool) -> bool {
    match kind {
        MenuKind::Check => !checked,
        MenuKind::Radio => true,
        _ => checked,
    }
}

fn separator() -> Shown {
    Shown {
        label: String::new(),
        enabled: false,
        kind: Kind::Separator,
    }
}

fn row_height(row: &Shown) -> i32 {
    if row.kind == Kind::Separator {
        SEPARATOR_H
    } else {
        ROW_H
    }
}

/// The rows whose parent is `parent`, as shown.
fn level(rows: &[MenuRow], parent: u32) -> Vec<Shown> {
    rows.iter()
        .filter(|row| row.parent == parent)
        .map(|row| match row.kind {
            MenuKind::Separator => separator(),
            MenuKind::Submenu => Shown {
                label: row.label.clone(),
                enabled: row.enabled,
                kind: Kind::Submenu { id: row.id },
            },
            kind => Shown {
                label: row.label.clone(),
                enabled: row.enabled,
                kind: Kind::Item {
                    id: row.id,
                    kind,
                    checked: row.checked,
                },
            },
        })
        .collect()
}
