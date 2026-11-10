//! [`SortedTable`] behind a real [`ListView`] on xui's offscreen backend:
//! header clicks sort by value, flip on a second click, keep the selection on
//! its item, and survive a refresh. The last frame is saved as
//! `target/snapshots/table-sorted.png` for a human to look at.

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::app::{App, Ui};
use xui_core::prelude::*;
use xui_core::Dip;

use super::SortedTable;

#[derive(Clone, Copy, Debug)]
enum Msg {
    Sort,
}

struct Table {
    list: Handle<ListView<Msg>>,
    sort: Rc<SortedTable>,
}

impl App for Table {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        match msg {
            Msg::Sort => self.sort.resort(&self.list.get()),
        }
    }
}

/// The app's rows: names out of order, sizes whose text order is wrong.
fn rows() -> Vec<Vec<String>> {
    [("b10", "1.5 MiB"), ("b2", "900 KiB"), ("a1", "12 B")]
        .iter()
        .map(|(name, size)| vec![(*name).to_owned(), (*size).to_owned()])
        .collect()
}

fn name(row: usize) -> &'static str {
    ["b10", "b2", "a1"][row]
}

/// The header's middle, at 96 dpi (the header is 24 px, columns 100 px).
fn header(column: i32) -> (i32, i32) {
    (column * 100 + 50, 12)
}

#[test]
fn header_clicks_sort_and_keep_the_selection() {
    let handle = Handle::new();
    let sort = Rc::new(SortedTable::new());
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let (app_list, app_sort) = (handle.clone(), Rc::clone(&sort));
    let (step_list, step_sort, step_seen) = (handle, Rc::clone(&sort), Rc::clone(&seen));
    let image = render_with(
        Snapshot::new(Dip(300.0), Dip(200.0)),
        move |ui| {
            ui.root(
                column().child(
                    list()
                        .column("Name", 100)
                        .column_right("Size", 100)
                        .then(|list| list.on_sort(|_| Some(Msg::Sort)))
                        .bind(&app_list)
                        .fill(1),
                ),
            )?;
            app_sort.show(&app_list.get(), rows());
            // Select `b2`, the app's row 1.
            app_list.get().select(Some(1));
            Ok(Table {
                list: app_list,
                sort: app_sort,
            })
        },
        move |stage| {
            let record = |what: &str| {
                let list = step_list.get();
                let order: Vec<&str> = (0..3)
                    .map(|row| name(step_sort.source(row).unwrap()))
                    .collect();
                let selected = list.selected().and_then(|row| step_sort.source(row));
                step_seen.borrow_mut().push(format!(
                    "{what}: {order:?} arrow={:?} selected={:?}",
                    list.sort_indicator(),
                    selected.map(name)
                ));
            };
            for (what, column) in [("name", 0), ("name again", 0), ("size", 1)] {
                let (x, y) = header(column);
                stage.click(x, y);
                record(what);
            }
            // A live table refreshes with the same rows: the order holds.
            step_sort.show(&step_list.get(), rows());
            record("refresh");
        },
    )
    .expect("the offscreen render");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join("table-sorted.png")).unwrap();
    let seen = seen.borrow();
    assert_eq!(
        *seen,
        [
            "name: [\"a1\", \"b2\", \"b10\"] arrow=Some((0, Ascending)) selected=Some(\"b2\")",
            "name again: [\"b10\", \"b2\", \"a1\"] arrow=Some((0, Descending)) selected=Some(\"b2\")",
            "size: [\"a1\", \"b2\", \"b10\"] arrow=Some((1, Ascending)) selected=Some(\"b2\")",
            "refresh: [\"a1\", \"b2\", \"b10\"] arrow=Some((1, Ascending)) selected=Some(\"b2\")",
        ]
    );
}
