//! The icon and details views: switching keeps the selection, the details
//! rows carry the columns, and the sort menu and the headers re-order both.

use std::cell::RefCell;
use std::rc::Rc;

use xui_explorer::MemPlatform;
use xui_explorer::model::SortKey;
use xui_explorer::window::{Msg, sort_menu_id};

use super::harness::{Handles, TestLauncher, drive, slash};

fn sample() -> Rc<MemPlatform> {
    Rc::new(
        MemPlatform::new()
            .dir("/d")
            .dir("/d/zeta")
            .dir("/d/Alpha")
            .file("/d/big.bin", 9_000)
            .file("/d/a.txt", 10)
            .file("/d/mid.png", 500),
    )
}

/// The details view's Name column, top to bottom.
fn names(handles: &Handles) -> String {
    (0..handles.details.len())
        .filter_map(|row| handles.details.cell_text(row, 0))
        .collect::<Vec<_>>()
        .join(",")
}

fn run(
    step: impl FnOnce(&xui_canvas::snapshot::Stage<'_, Msg>, &Handles, &dyn Fn(String)) + 'static,
) -> Vec<String> {
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = Rc::clone(&seen);
    drive(
        sample(),
        Rc::new(TestLauncher::default()),
        "/d",
        move |stage, handles| {
            let record = |line: String| log.borrow_mut().push(line);
            step(stage, handles, &record);
        },
    );
    seen.take()
}

#[test]
fn the_details_rows_carry_name_size_type_and_modified() {
    let seen = run(|stage, handles, record| {
        stage.emit(Msg::ToggleView);
        for row in [0, 3] {
            let cells: Vec<String> = (0..4)
                .map(|column| handles.details.cell_text(row, column).unwrap_or_default())
                .collect();
            record(cells.join("|"));
        }
    });
    assert_eq!(seen, ["Alpha||Folder|", "big.bin|8.8 KiB|BIN File|"]);
}

#[test]
fn switching_views_keeps_the_selection_and_the_published_view() {
    let seen = run(|stage, handles, record| {
        handles.icons.set_selection(&[2, 4]);
        stage.emit(Msg::Selection);
        let icons = handles
            .explorer
            .view_state(handles.window.raw())
            .unwrap()
            .view;
        stage.emit(Msg::ToggleView);
        let details = handles
            .explorer
            .view_state(handles.window.raw())
            .unwrap()
            .view;
        record(format!(
            "{:?} {} {}",
            handles.details.selection(),
            icons == handles.icons.id(),
            details == handles.details.id()
        ));
        handles.details.set_selection(&[0]);
        stage.emit(Msg::Selection);
        stage.emit(Msg::ToggleView);
        record(format!("{:?}", handles.icons.selection()));
    });
    assert_eq!(seen, ["[2, 4] true true", "[0]"]);
}

#[test]
fn a_header_click_sorts_and_a_second_click_reverses() {
    let seen = run(|stage, handles, record| {
        stage.emit(Msg::ToggleView);
        record(names(handles));
        stage.emit(Msg::SortColumn(1)); // Size
        record(names(handles));
        stage.emit(Msg::SortColumn(1));
        record(names(handles));
        stage.emit(Msg::SortColumn(0)); // back to Name, ascending
        record(names(handles));
    });
    assert_eq!(
        seen,
        [
            "Alpha,zeta,a.txt,big.bin,mid.png",
            "Alpha,zeta,a.txt,mid.png,big.bin",
            "Alpha,zeta,big.bin,mid.png,a.txt",
            "Alpha,zeta,a.txt,big.bin,mid.png",
        ]
    );
}

#[test]
fn the_sort_menu_reorders_the_icons_and_keeps_the_selection() {
    let seen = run(|stage, handles, record| {
        handles.icons.set_selection(&[2]); // a.txt
        stage.emit(Msg::Selection);
        stage.emit(Msg::SortChosen(sort_menu_id(SortKey::Size)));
        stage.emit(Msg::SortChosen(xui_explorer::window::SORT_DESCENDING));
        let selected: Vec<String> = handles.selected().iter().map(|path| slash(path)).collect();
        record(selected.join(","));
        stage.emit(Msg::ToggleView);
        record(names(handles));
        record(format!("{:?}", handles.details.sort_indicator()));
    });
    assert_eq!(
        seen,
        [
            "/d/a.txt",
            "Alpha,zeta,big.bin,mid.png,a.txt",
            "Some((1, Descending))",
        ]
    );
}

#[test]
fn up_in_the_details_view_selects_the_folder_we_came_from() {
    let seen = run(|stage, handles, record| {
        stage.emit(Msg::Activate(1)); // zeta
        stage.emit(Msg::ToggleView);
        stage.emit(Msg::Up);
        record(format!("{:?}", handles.details.selection()));
        let selected: Vec<String> = handles.selected().iter().map(|path| slash(path)).collect();
        record(selected.join(","));
    });
    assert_eq!(seen, ["[1]", "/d/zeta"]);
}

/// `Ui::on_key` cannot consume a key: after Alt+Up the focused view gets the
/// Up arrow too and moves its selection, which the window undoes.
#[test]
fn the_arrow_of_alt_up_does_not_move_the_selection_after_it() {
    use xui_core::message::{Key, Modifiers};

    let seen = run(|stage, handles, record| {
        stage.emit(Msg::Activate(1)); // zeta
        stage.emit(Msg::ToggleView);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        stage.emit(Msg::Key(Key::UP, alt));
        // What the list does with the same key, in the same event.
        handles.details.set_selection(&[0]);
        stage.emit(Msg::Selection);
        record(format!("{:?}", handles.details.selection()));
        // A later, real selection change goes through.
        handles.details.set_selection(&[0]);
        stage.emit(Msg::Selection);
        record(format!("{:?}", handles.details.selection()));
    });
    assert_eq!(seen, ["[1]", "[0]"]);
}

#[test]
fn the_sort_menu_closes_on_any_other_click_and_on_a_second_sort_click() {
    let seen = run(|stage, handles, record| {
        let popup = handles.sort_popup.expect("the sort menu has a popup");
        let open = || stage.ui().is_visible(popup);
        stage.emit(Msg::SortMenu);
        record(format!("{}", open()));
        stage.emit(Msg::Selection); // a click in the view
        record(format!("{}", open()));
        stage.emit(Msg::SortMenu);
        stage.emit(Msg::SortMenu); // the button again
        record(format!("{}", open()));
        stage.emit(Msg::SortMenu);
        stage.emit(Msg::Up); // a toolbar button
        record(format!("{}", open()));
    });
    assert_eq!(seen, ["true", "false", "false", "false"]);
}
