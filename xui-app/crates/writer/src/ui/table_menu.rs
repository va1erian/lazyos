#![forbid(unsafe_code)]

//! The Table menu, shown under the Table button: an Insert table submenu of
//! sizes, then the row, column and table commands and the Header row and
//! Borders switches, enabled while the caret is in a table.

use xui_core::app::Ui;
use xui_core::widget::{Lucide, Menu, MenuId};
use xui_rich_text::edit::{Command, TableCursor};

use crate::app::Msg;

/// The sizes Insert table offers, columns by rows.
pub const SIZES: [(usize, usize); 4] = [(2, 2), (3, 3), (4, 4), (5, 5)];

/// The first Insert table entry; one per [`SIZES`] entry follows it.
pub const INSERT: usize = 1;
/// Insert row above; the entries below follow it in menu order.
pub const ROW_ABOVE: usize = 10;
const ROW_BELOW: usize = 11;
const COLUMN_LEFT: usize = 12;
const COLUMN_RIGHT: usize = 13;
const DELETE_ROWS: usize = 14;
const DELETE_COLUMNS: usize = 15;
const DELETE_TABLE: usize = 16;
/// The Header row switch.
pub const HEADER: usize = 17;
const BORDERS: usize = 18;
const INSERT_MENU: usize = 20;

/// The menu.
pub fn build(ui: &Ui<Msg>) -> Menu<Msg> {
    let id = MenuId::new;
    Menu::context(ui)
        .build(|m| {
            m.submenu(id(INSERT_MENU), "Insert table", |m| {
                for (i, (columns, rows)) in SIZES.into_iter().enumerate() {
                    m.item(id(INSERT + i), &format!("{columns} x {rows}"));
                }
            })
            .icon(Lucide::Table);
            m.separator();
            m.item(id(ROW_ABOVE), "Insert row above")
                .icon(Lucide::BetweenHorizontalStart);
            m.item(id(ROW_BELOW), "Insert row below")
                .icon(Lucide::BetweenHorizontalEnd);
            m.item(id(COLUMN_LEFT), "Insert column left")
                .icon(Lucide::BetweenVerticalStart);
            m.item(id(COLUMN_RIGHT), "Insert column right")
                .icon(Lucide::BetweenVerticalEnd);
            m.separator();
            m.item(id(DELETE_ROWS), "Delete rows");
            m.item(id(DELETE_COLUMNS), "Delete columns");
            m.item(id(DELETE_TABLE), "Delete table")
                .icon(Lucide::Trash2);
            m.separator();
            m.check(id(HEADER), "Header row", false);
            m.check(id(BORDERS), "Borders", false);
        })
        .on_select(|id| index_of(id).map(|i| Msg::TableChoice(i, true)))
        .on_toggle(|id, on| index_of(id).map(|i| Msg::TableChoice(i, on)))
}

/// The entry index of menu id `id`.
fn index_of(id: MenuId) -> Option<usize> {
    (INSERT..=BORDERS).find(|&i| MenuId::new(i) == id)
}

/// Enables the table commands while the caret is in a table (`cursor`) and
/// Insert table outside one, and checks the table's switches.
pub fn sync(menu: &Menu<Msg>, cursor: Option<&TableCursor>) {
    menu.set_enabled(MenuId::new(INSERT_MENU), cursor.is_none());
    for index in ROW_ABOVE..=BORDERS {
        menu.set_enabled(MenuId::new(index), cursor.is_some());
    }
    menu.set_checked(MenuId::new(HEADER), cursor.is_some_and(|c| c.table.header));
    menu.set_checked(MenuId::new(BORDERS), cursor.is_some_and(|c| c.table.border));
}

/// The editor command for entry `index`, `on` being a switch's new state;
/// the switches need the caret's table.
pub fn command(index: usize, on: bool, cursor: Option<TableCursor>) -> Option<Command> {
    Some(match index {
        ROW_ABOVE => Command::InsertRow { below: false },
        ROW_BELOW => Command::InsertRow { below: true },
        COLUMN_LEFT => Command::InsertColumn { right: false },
        COLUMN_RIGHT => Command::InsertColumn { right: true },
        DELETE_ROWS => Command::DeleteRows,
        DELETE_COLUMNS => Command::DeleteColumns,
        DELETE_TABLE => Command::DeleteTable,
        HEADER | BORDERS => {
            let cursor = cursor?;
            let mut table = cursor.table;
            if index == HEADER {
                table.header = on;
            } else {
                table.border = on;
            }
            Command::SetTable {
                id: cursor.id,
                table,
            }
        }
        _ => {
            let (columns, rows) = *SIZES.get(index.checked_sub(INSERT)?)?;
            Command::InsertTable { rows, columns }
        }
    })
}

/// The status bar's table label: `Table: row 2, column 3`, or empty outside
/// a table.
pub fn cell_label(cursor: Option<&TableCursor>) -> String {
    cursor.map_or_else(String::new, |c| {
        format!("Table: row {}, column {}", c.row + 1, c.column + 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_rich_text::model::{Table, TableTable};

    fn cursor() -> TableCursor {
        TableCursor {
            id: TableTable::new().next_id(),
            row: 1,
            column: 2,
            rows: 3,
            table: Table::new(3),
        }
    }

    #[test]
    fn each_size_inserts_its_table() {
        assert!(matches!(
            command(INSERT + 1, true, None),
            Some(Command::InsertTable {
                rows: 3,
                columns: 3
            })
        ));
        assert!(command(INSERT + SIZES.len(), true, None).is_none());
    }

    #[test]
    fn the_switches_change_the_caret_table() {
        assert!(command(HEADER, true, None).is_none());
        let Some(Command::SetTable { table, .. }) = command(HEADER, true, Some(cursor())) else {
            panic!("a table change");
        };
        assert!(table.header && table.border);
        let Some(Command::SetTable { table, .. }) = command(BORDERS, false, Some(cursor())) else {
            panic!("a table change");
        };
        assert!(!table.border);
    }

    #[test]
    fn the_status_names_the_caret_cell() {
        assert_eq!(cell_label(None), "");
        assert_eq!(cell_label(Some(&cursor())), "Table: row 2, column 3");
    }
}
