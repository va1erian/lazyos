//! Tables in LazyWriter: the Table menu inserts and edits a table, the status
//! bar names the caret's cell, and a saved table opens again. The window with
//! a table is saved as `xui-app/target/snapshots/writer-table-{light,dark}.png`
//! for a human to look at.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::Stage;
use xui_core::Theme;
use xui_rich_text::edit::Command;
use xui_writer::Msg;
use xui_writer::ui::table_menu::{HEADER, INSERT, ROW_ABOVE};

use common::{Rig, TempDir, pump, render, save, watchdog};

/// The Insert table entry for 3 x 3.
const THREE_BY_THREE: usize = INSERT + 1;

/// "Before", a 3 x 3 table with a header row, and "after"; the caret ends in
/// the middle cell.
fn fill(stage: &Stage<'_, Msg>, rig: &Rig) {
    let e = &rig.editor;
    e.exec(Command::InsertText(
        "Before the table\nAfter the table".into(),
    ));
    e.exec(Command::SetCaret {
        pos: xui_rich_text::DocPos::new(1, 0),
        extend: false,
    });
    stage.emit(Msg::TableChoice(THREE_BY_THREE, true));
    let cells = [
        "Item", "Owner", "Due", "Plan", "Valerian", "Monday", "Build", "Claude", "Friday",
    ];
    for (i, text) in cells.iter().enumerate() {
        if i > 0 {
            e.exec(Command::NextCell);
        }
        e.exec(Command::InsertText((*text).into()));
    }
    stage.emit(Msg::TableChoice(HEADER, true));
    for _ in 0..4 {
        e.exec(Command::PrevCell);
    }
    pump(stage);
}

/// Runs `step` in a live window; returns what it recorded.
fn drive(
    step: impl FnOnce(&Stage<'_, Msg>, &Rig, &TempDir) -> Vec<String> + Send + 'static,
) -> Vec<String> {
    watchdog(|| {
        let dir = Rc::new(TempDir::new("tables"));
        let out: Rc<RefCell<Vec<String>>> = Rc::default();
        let (keep, folder) = (Rc::clone(&out), Rc::clone(&dir));
        render(Theme::light(), dir.0.clone(), move |stage, rig| {
            *keep.borrow_mut() = step(stage, rig, &folder);
        });
        out.take()
    })
}

#[test]
fn the_table_menu_inserts_a_table_and_the_status_follows_the_caret() {
    let seen = drive(|stage, rig, _| {
        let before = rig.status(4);
        fill(stage, rig);
        let middle = rig.status(4);
        stage.emit(Msg::TableChoice(ROW_ABOVE, true));
        pump(stage);
        let rows = rig.editor.with_document(|d| d.table_spans()[0].rows.len());
        let header = rig.editor.table_cursor().is_some_and(|c| c.table.header);
        rig.editor.exec(Command::SetCaret {
            pos: xui_rich_text::DocPos::new(0, 0),
            extend: false,
        });
        pump(stage);
        vec![
            before,
            middle,
            rows.to_string(),
            header.to_string(),
            rig.status(4),
        ]
    });
    assert_eq!(
        seen,
        ["", "Table: row 2, column 2", "4", "true", ""],
        "Insert row above adds a fourth row"
    );
}

#[test]
fn a_saved_table_opens_again() {
    let seen = drive(|stage, rig, dir| {
        fill(stage, rig);
        let path = dir.file("table.lzw");
        stage.emit(Msg::SaveChosen(path.clone()));
        pump(stage);
        stage.emit(Msg::New);
        pump(stage);
        stage.emit(Msg::OpenChosen(path));
        pump(stage);
        rig.editor.with_document(|d| {
            let span = &d.table_spans()[0];
            let table = d.tables().get(span.id).expect("the table");
            vec![
                format!("{}x{}", span.columns(), span.rows.len()),
                table.header.to_string(),
                table.border.to_string(),
            ]
        })
    });
    assert_eq!(seen, ["3x3", "true", "true"]);
}

#[test]
fn the_window_with_a_table_renders_in_light_and_dark() {
    let images = [Theme::light(), Theme::dark()].map(|theme| {
        watchdog(move || {
            let dir = TempDir::new("table-snapshot");
            render(theme, dir.0.clone(), fill)
        })
    });
    save(&images[0], "writer-table-light.png");
    save(&images[1], "writer-table-dark.png");
    assert_ne!(images[0].pixels(), images[1].pixels());
}
