//! The list's cells: rows formatted once per refresh into the `ListModel`
//! the view paints from.

use xui_core::icon::IconRef;
use xui_core::widget::{ListModel, Lucide};

use crate::folder::{Column, Row, RowKind};

/// A byte count the way a file manager shows it.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// One formatted row.
fn cells(row: &Row) -> [String; 5] {
    Column::ALL.map(|column| match column {
        Column::Name => {
            if row.encrypted {
                format!("{} *", row.name)
            } else {
                row.name.clone()
            }
        }
        Column::Size if row.kind == RowKind::Parent => String::new(),
        Column::Size => size(row.size),
        Column::Packed => match (row.kind, row.packed) {
            (RowKind::Parent, _) | (_, None) => String::new(),
            (_, Some(packed)) => size(packed),
        },
        Column::Modified => row.modified.map(lazyarc::time::display).unwrap_or_default(),
        Column::Method => row.method.clone(),
    })
}

/// The rows as the list view's model.
pub struct Cells {
    rows: Vec<[String; 5]>,
    kinds: Vec<RowKind>,
}

impl Cells {
    pub fn new(rows: &[Row]) -> Cells {
        Cells {
            rows: rows.iter().map(cells).collect(),
            kinds: rows.iter().map(|row| row.kind).collect(),
        }
    }
}

impl ListModel for Cells {
    fn rows(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&str> {
        self.rows.get(row)?.get(column).map(String::as_str)
    }

    fn icon(&self, row: usize) -> Option<IconRef> {
        Some(match self.kinds.get(row)? {
            RowKind::Parent => Lucide::ChevronUp.into(),
            RowKind::Folder => Lucide::Folder.into(),
            RowKind::Link => Lucide::Link.into(),
            RowKind::File => Lucide::File.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(50 * 1024 * 1024), "50 MB");
    }

    #[test]
    fn the_parent_row_has_no_size() {
        let cells = Cells::new(&[crate::folder::parent_row()]);
        assert_eq!(cells.cell(0, 0), Some(".."));
        assert_eq!(cells.cell(0, 1), Some(""));
    }
}
