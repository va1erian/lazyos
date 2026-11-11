//! Header-click sorting for report lists an app fills with text rows.
//!
//! A [`ListView`] moves its own sort arrow when a header is clicked and then
//! raises the app's `on_sort` message; the app owns the order of the rows.
//! [`SortedTable`] is that order for a list of text cells: it keeps the rows
//! as the app gave them, shows them sorted by the arrow, and maps a shown row
//! back to the app's row so an index-based selection or activation still
//! names the right item. Live tables call [`SortedTable::show`] on every
//! refresh and the arrow's order survives it.
//!
//! Cells compare the way a person reads them ([`cell_cmp`]): numbers and
//! sizes (`9`, `1.5 MiB`, `40%`) by value, everything else in natural order
//! (`file2` before `file10`, case ignored). An app with its own notion of a
//! row's key uses [`sorted_order`] with an accessor instead.

use std::cell::RefCell;
use std::cmp::Ordering;

use xui_core::natural_cmp;
use xui_core::widget::{ListView, SortDirection};

/// Text rows shown in a list's header order. Its methods take `&self`, like
/// a widget handle, so it sits beside one in an app's widget struct.
#[derive(Default)]
pub struct SortedTable {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    rows: Vec<Vec<String>>,
    /// What each cell sorts by, when that is not its text (empty: the text).
    keys: Vec<Vec<String>>,
    /// Shown row -> the app's row.
    order: Vec<usize>,
}

impl SortedTable {
    pub fn new() -> SortedTable {
        SortedTable::default()
    }

    /// Shows `rows` (in the app's order) sorted by `list`'s arrow. Like
    /// [`ListView::refresh_model`], the scroll position and the selected row
    /// numbers are kept, so a periodic refresh does not move the view.
    pub fn show<M: 'static>(&self, list: &ListView<M>, rows: Vec<Vec<String>>) {
        self.show_keyed(list, rows, Vec::new());
    }

    /// [`show`](Self::show), sorting by `keys` (one row of sort text per
    /// row, cell for cell) instead of what the cells say: a signal shown as
    /// bars sorts by its dBm, a sender by the name without an unread mark.
    pub fn show_keyed<M: 'static>(
        &self,
        list: &ListView<M>,
        rows: Vec<Vec<String>>,
        keys: Vec<Vec<String>>,
    ) {
        let shown = {
            let mut state = self.state.borrow_mut();
            state.rows = rows;
            state.keys = keys;
            state.order = state.order_for(list.sort_indicator());
            state.shown_rows()
        };
        list.refresh_model(shown);
    }

    /// Empties the list (and the table).
    pub fn clear<M: 'static>(&self, list: &ListView<M>) {
        self.show(list, Vec::new());
    }

    /// A header click: sorts the rows shown by the arrow `list` now has,
    /// carrying the selection and focus with their items.
    pub fn resort<M: 'static>(&self, list: &ListView<M>) {
        let (rows, before, shown) = {
            let mut state = self.state.borrow_mut();
            let before = std::mem::take(&mut state.order);
            state.order = state.order_for(list.sort_indicator());
            (state.shown_rows(), before, inverse(&state.order))
        };
        list.reorder_model(rows, |old| {
            before.get(old).and_then(|&row| shown.get(row).copied())
        });
    }

    /// The app's row for shown row `shown`.
    pub fn source(&self, shown: usize) -> Option<usize> {
        self.state.borrow().order.get(shown).copied()
    }

    /// The shown row of the app's row `source`.
    pub fn shown(&self, source: usize) -> Option<usize> {
        self.state
            .borrow()
            .order
            .iter()
            .position(|&row| row == source)
    }
}

impl State {
    fn order_for(&self, sort: Option<(usize, SortDirection)>) -> Vec<usize> {
        let by = if self.keys.is_empty() {
            &self.rows
        } else {
            &self.keys
        };
        sorted_order(self.rows.len(), sort, |row, column| cell(by, row, column))
    }

    fn shown_rows(&self) -> Vec<Vec<String>> {
        self.order
            .iter()
            .map(|&row| self.rows[row].clone())
            .collect()
    }
}

/// The order to show `len` rows in for `sort` (the list's arrow), as shown
/// row -> the app's row. `cell(row, column)` reads a row's sort text. Equal
/// cells keep the app's order, so a sort never shuffles ties.
pub fn sorted_order<'a>(
    len: usize,
    sort: Option<(usize, SortDirection)>,
    cell: impl Fn(usize, usize) -> &'a str,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    if let Some((column, direction)) = sort {
        order.sort_by(|&a, &b| {
            let by = cell_cmp(cell(a, column), cell(b, column));
            match direction {
                SortDirection::Ascending => by,
                SortDirection::Descending => by.reverse(),
            }
        });
    }
    order
}

/// Compares two cells: every number or size sorts before every other text,
/// numbers by value (then as text, so `1.0` and `1` stay ordered), other text
/// in natural order. A total order, as a sort requires.
pub fn cell_cmp(a: &str, b: &str) -> Ordering {
    match (quantity(a), quantity(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y).then_with(|| natural_cmp(a, b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => natural_cmp(a, b),
    }
}

/// The value of a cell that is a number, optionally followed by a size or
/// time unit or `%` (`12`, `-3`, `1.5 MiB`, `40%`, `250 ms`), or a clock
/// time in seconds (`3:07`, `1:02:03`).
fn quantity(text: &str) -> Option<f64> {
    let text = text.trim();
    if let Some(seconds) = clock(text) {
        return Some(seconds);
    }
    let end = text
        .char_indices()
        .find(|&(at, c)| !(c.is_ascii_digit() || c == '.' || (c == '-' && at == 0)))
        .map_or(text.len(), |(at, _)| at);
    let (number, unit) = text.split_at(end);
    if !number.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: f64 = number.parse().ok()?;
    Some(value * scale(unit.trim())?)
}

/// `M:SS` or `H:MM:SS` in seconds.
fn clock(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut seconds = 0.0;
    for part in parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(seconds)
}

/// What one of `unit` is worth, for the units the apps print.
fn scale(unit: &str) -> Option<f64> {
    const K: f64 = 1024.0;
    Some(match unit {
        "" | "%" | "B" | "s" => 1.0,
        "KiB" | "KB" | "kB" | "K" => K,
        "MiB" | "MB" | "M" => K * K,
        "GiB" | "GB" | "G" => K * K * K,
        "TiB" | "TB" | "T" => K * K * K * K,
        "ms" => 1e-3,
        "us" | "µs" => 1e-6,
        _ => return None,
    })
}

fn cell(rows: &[Vec<String>], row: usize, column: usize) -> &str {
    rows.get(row)
        .and_then(|cells| cells.get(column))
        .map_or("", String::as_str)
}

/// `order` (shown -> source) turned around (source -> shown).
fn inverse(order: &[usize]) -> Vec<usize> {
    let mut shown = vec![0; order.len()];
    for (at, &row) in order.iter().enumerate() {
        shown[row] = at;
    }
    shown
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod click_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn sort(cells: &[&str], direction: SortDirection) -> Vec<String> {
        let order = sorted_order(cells.len(), Some((0, direction)), |row, _| cells[row]);
        order.iter().map(|&row| cells[row].to_owned()).collect()
    }

    #[test]
    fn sizes_and_numbers_sort_by_value() {
        let cells = ["1.5 MiB", "900 KiB", "12 B", "2.0 GiB", "—"];
        assert_eq!(
            sort(&cells, SortDirection::Ascending),
            ["12 B", "900 KiB", "1.5 MiB", "2.0 GiB", "—"]
        );
        let cells = ["100", "9", "-3", "40%", "250 ms"];
        assert_eq!(
            sort(&cells, SortDirection::Descending),
            ["100", "40%", "9", "250 ms", "-3"]
        );
    }

    #[test]
    fn text_sorts_naturally_and_ties_keep_the_app_order() {
        let cells = ["task10", "Task2", "0x1f", "task1", "v1.2.3"];
        assert_eq!(
            sort(&cells, SortDirection::Ascending),
            ["0x1f", "task1", "Task2", "task10", "v1.2.3"]
        );
        let cells = ["1:02:03", "59:00", "0:07"];
        assert_eq!(
            sort(&cells, SortDirection::Ascending),
            ["0:07", "59:00", "1:02:03"]
        );
        let order = sorted_order(3, Some((0, SortDirection::Descending)), |row, _| {
            ["same", "same", "same"][row]
        });
        assert_eq!(order, [0, 1, 2]);
        assert_eq!(sorted_order(3, None, |_, _| ""), [0, 1, 2]);
    }

    #[test]
    fn quantities_parse_only_whole_cells() {
        assert_eq!(quantity("1.5 MiB"), Some(1.5 * 1024.0 * 1024.0));
        assert_eq!(quantity(" 7 "), Some(7.0));
        assert_eq!(quantity("-"), None);
        assert_eq!(quantity("1.2.3"), None);
        assert_eq!(quantity("12 apples"), None);
        assert_eq!(quantity("3-4"), None);
        assert_eq!(quantity(""), None);
        assert_eq!(quantity("1:02:03"), Some(3723.0));
        assert_eq!(quantity("1::3"), None);
        assert_eq!(quantity("a:b"), None);
    }

    #[test]
    fn keys_sort_in_place_of_the_text() {
        let state = State {
            rows: vec![vec!["[##..] -70 dBm".into()], vec!["[####] -40 dBm".into()]],
            keys: vec![vec!["-70".into()], vec!["-40".into()]],
            order: Vec::new(),
        };
        let descending = Some((0, SortDirection::Descending));
        assert_eq!(state.order_for(descending), [1, 0]);
        let missing = State {
            keys: vec![vec!["1".into()]],
            ..state
        };
        // A key row shorter than the table sorts the rest as empty text.
        assert_eq!(
            missing.order_for(Some((0, SortDirection::Ascending))),
            [0, 1]
        );
    }

    #[test]
    fn the_order_maps_both_ways() {
        let order = [2, 0, 1];
        assert_eq!(inverse(&order), [1, 2, 0]);
    }
}
