//! In-place navigation: opening a folder, Back, Forward, Up, the address
//! bar, the keyboard shortcuts and the title.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::message::{Key, Modifiers};
use xui_core::widget::HasText;
use xui_explorer::MemPlatform;
use xui_explorer::platform::Platform;
use xui_explorer::window::Msg;

use super::harness::{Handles, TestLauncher, drive, has, mem, slash, slashed};

/// A tree three folders deep with a file at each level.
fn deep() -> Rc<MemPlatform> {
    Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .dir("/a/b/c")
            .file("/a/top.txt", 1)
            .file("/a/b/mid.txt", 1)
            .file("/a/b/c/low.txt", 1)
            .dir("/locked")
            .unreadable("/locked"),
    )
}

/// Runs `step` on a window at `start` over [`deep`], recording `where(...)`
/// snapshots of the folder and title it pushes.
fn walk(
    start: &str,
    step: impl FnOnce(&xui_canvas::snapshot::Stage<'_, Msg>, &Handles, &dyn Fn(&Handles)) + 'static,
) -> (Vec<String>, Handles) {
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = Rc::clone(&seen);
    let (_, handles) = drive(
        deep(),
        Rc::new(TestLauncher::default()),
        start,
        move |stage, handles| {
            let record = |handles: &Handles| {
                log.borrow_mut().push(format!(
                    "{} [{}] {}",
                    slash(&handles.dir()),
                    handles.title(),
                    slashed(&handles.address.text())
                ));
            };
            step(stage, handles, &record);
        },
    );
    let seen = seen.borrow().clone();
    (seen, handles)
}

#[test]
fn opening_a_folder_replaces_the_view_in_the_same_window() {
    let (seen, handles) = walk("/a", |stage, handles, record| {
        record(handles);
        stage.emit(Msg::Activate(0)); // b
        record(handles);
        stage.emit(Msg::Activate(0)); // c
        record(handles);
    });
    assert_eq!(seen, ["/a [a] /a", "/a/b [b] /a/b", "/a/b/c [c] /a/b/c",]);
    assert_eq!(
        handles.explorer.open_dirs().len(),
        0,
        "closed after the run"
    );
}

#[test]
fn back_and_forward_walk_the_history() {
    let (seen, _) = walk("/a", |stage, handles, record| {
        stage.emit(Msg::Activate(0)); // b
        stage.emit(Msg::Activate(0)); // c
        stage.emit(Msg::Back);
        record(handles);
        stage.emit(Msg::Back);
        record(handles);
        stage.emit(Msg::Back); // nothing behind /a
        record(handles);
        stage.emit(Msg::Forward);
        record(handles);
        stage.emit(Msg::Activate(1)); // mid.txt opens a file: no visit
        stage.emit(Msg::Forward);
        record(handles);
    });
    assert_eq!(
        seen,
        [
            "/a/b [b] /a/b",
            "/a [a] /a",
            "/a [a] /a",
            "/a/b [b] /a/b",
            "/a/b/c [c] /a/b/c",
        ]
    );
}

#[test]
fn up_goes_to_the_parent_with_the_folder_selected_and_stops_at_the_root() {
    let (seen, _) = walk("/a/b/c", |stage, handles, record| {
        stage.emit(Msg::Up);
        record(handles);
        let selected = handles.selected();
        assert_eq!(selected, [PathBuf::from("/a/b/c")], "came from c");
        stage.emit(Msg::Up);
        stage.emit(Msg::Up);
        record(handles);
        stage.emit(Msg::Up); // nothing above the root
        record(handles);
        stage.emit(Msg::Back);
        record(handles);
    });
    assert_eq!(seen, ["/a/b [b] /a/b", "/ [/] /", "/ [/] /", "/a [a] /a"]);
}

#[test]
fn a_folder_that_cannot_be_listed_is_not_entered() {
    let (seen, handles) = walk("/", |stage, handles, record| {
        // Folders first: a, locked.
        stage.emit(Msg::Activate(1));
        record(handles);
    });
    assert_eq!(seen, ["/ [/] /"]);
    assert!(has(handles.status.text(0), "Cannot open /locked"));
}

#[test]
fn the_address_bar_opens_folders_and_files_and_reports_the_rest() {
    let launcher = Rc::new(TestLauncher::default());
    let opened = Rc::clone(&launcher);
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = Rc::clone(&seen);
    let (_, handles) = drive(
        deep() as Rc<dyn Platform>,
        launcher,
        "/a",
        move |stage, handles| {
            let go = |text: &str| {
                handles.address.set_text(text);
                stage.emit(Msg::AddressEdited);
                stage.emit(Msg::Go);
                log.borrow_mut().push(format!(
                    "{} {}",
                    slash(&handles.dir()),
                    slashed(&handles.address.text())
                ));
            };
            go("b/c");
            go("..");
            go("/a/top.txt");
            go("/nowhere");
        },
    );
    assert_eq!(
        *seen.borrow(),
        ["/a/b/c /a/b/c", "/a/b /a/b", "/a/b /a/b", "/a/b /nowhere"]
    );
    assert_eq!(*opened.opened.borrow(), [PathBuf::from("/a/top.txt")]);
    assert!(has(handles.status.text(0), "Cannot find"));
    assert!(has(handles.status.text(0), "nowhere"));
}

#[test]
fn escape_restores_the_address_and_editing_keys_stay_in_it() {
    let (seen, _) = walk("/a/b", |stage, handles, record| {
        let none = Modifiers::NONE;
        handles.address.set_text("/a/b/zz");
        stage.emit(Msg::AddressEdited);
        // Backspace and Delete edit the address, not the folder.
        stage.emit(Msg::Key(Key::BACK, none));
        stage.emit(Msg::Key(Key::DELETE, none));
        record(handles);
        stage.emit(Msg::Key(Key::ESCAPE, none));
        record(handles);
        // With the edit gone, Backspace goes up.
        stage.emit(Msg::Key(Key::BACK, none));
        record(handles);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        stage.emit(Msg::Key(Key::LEFT, alt));
        record(handles);
        // Ctrl+L empties the bar for a new path; Escape puts the folder back.
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        stage.emit(Msg::Key(Key::L, ctrl));
        record(handles);
        stage.emit(Msg::Key(Key::ESCAPE, none));
        record(handles);
    });
    assert_eq!(
        seen,
        [
            "/a/b [b] /a/b/zz",
            "/a/b [b] /a/b",
            "/a [a] /a",
            "/a/b [b] /a/b",
            "/a/b [b] ",
            "/a/b [b] /a/b",
        ]
    );
}

#[test]
fn a_refresh_inside_a_deleted_folder_climbs_to_the_nearest_folder() {
    let platform = mem();
    let mutator = Rc::clone(&platform);
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = Rc::clone(&seen);
    drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a/b",
        move |stage, handles| {
            mutator
                .remove(std::path::Path::new("/a/b"), true)
                .expect("remove");
            stage.emit(Msg::Refresh);
            log.borrow_mut()
                .push(format!("{} [{}]", slash(&handles.dir()), handles.title()));
        },
    );
    assert_eq!(*seen.borrow(), ["/a [a]"]);
}

/// A folder whose tiles need the scrollbar, beside one whose do not.
fn tall() -> Rc<MemPlatform> {
    let mut platform = MemPlatform::new()
        .dir("/a")
        .dir("/a/big")
        .file("/a/x.txt", 1);
    for index in 0..60 {
        platform = platform.file(&format!("/a/big/f{index}.txt"), 1);
    }
    Rc::new(platform)
}

/// Regression: swapping the icon view's model when its scrollbar appears or
/// goes re-entered the layout and panicked inside xui (see `set_models`).
#[test]
fn moving_between_a_short_and_a_tall_folder_shows_and_hides_the_scrollbar() {
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = Rc::clone(&seen);
    drive(
        tall(),
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            stage.emit(Msg::Activate(0)); // big
            log.borrow_mut().push(format!("{}", handles.icons.len()));
            stage.emit(Msg::Up);
            stage.emit(Msg::ToggleView);
            stage.emit(Msg::Activate(0));
            stage.emit(Msg::ToggleView);
            stage.emit(Msg::Back);
            log.borrow_mut().push(format!("{}", handles.icons.len()));
        },
    );
    assert_eq!(*seen.borrow(), ["60", "2"]);
}

#[test]
fn back_or_forward_into_an_unreadable_folder_stays_and_keeps_the_history() {
    let platform = deep();
    let lock = Rc::clone(&platform);
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = Rc::clone(&seen);
    drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            let record = |handles: &Handles| {
                log.borrow_mut()
                    .push(format!("{} [{}]", slash(&handles.dir()), handles.title()));
            };
            stage.emit(Msg::Activate(0)); // b
            stage.emit(Msg::Back);
            lock.set_readable("/a/b", false);
            stage.emit(Msg::Forward); // refused: stays at /a
            record(handles);
            let refused = has(handles.status.text(0), "Cannot open");
            log.borrow_mut().push(format!("refused={refused}"));
            lock.set_readable("/a/b", true);
            stage.emit(Msg::Forward); // the step is still there
            record(handles);
            stage.emit(Msg::Back);
            record(handles);
        },
    );
    assert_eq!(
        *seen.borrow(),
        ["/a [a]", "refused=true", "/a/b [b]", "/a [a]"]
    );
}
