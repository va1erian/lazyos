#![forbid(unsafe_code)]

//! Headless UI tests: a window over an in-memory filesystem, driven through the
//! offscreen backend's synthetic input and messages. No real window opens and
//! the loop runs to completion synchronously, so there is no watchdog to arm.

#[path = "headless/flash.rs"]
mod flash;
#[path = "headless/harness.rs"]
mod harness;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::TaskDialogAction;
use xui_explorer::MemPlatform;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::window::Msg;

use harness::{TestLauncher, drive, has, mem};

#[test]
fn opening_an_already_open_folder_is_a_no_op_with_a_hint() {
    let (explorer, handles) = drive(
        mem(),
        Rc::new(TestLauncher::default()),
        "/a",
        |stage, handles| {
            handles.view.set_selection(&[0]);
            stage.emit(Msg::Activate(0));
            stage.emit(Msg::Activate(0));
        },
    );
    assert!(
        explorer.registry().is_open(Path::new("/a/b")),
        "the folder window is tracked"
    );
    assert!(has(handles.status.text(0), "already open"));
}

#[test]
fn opening_a_folder_hints_its_origin_once_and_a_duplicate_open_does_not() {
    let launcher = Rc::new(TestLauncher::default());
    let (_, handles) = drive(
        mem(),
        Rc::clone(&launcher) as Rc<dyn Launcher>,
        "/a",
        |stage, handles| {
            handles.view.set_selection(&[0]);
            stage.emit(Msg::Activate(0));
            stage.emit(Msg::Activate(0)); // already open: no second hint
        },
    );
    let hints = launcher.hints.borrow();
    assert_eq!(hints.len(), 1, "one hint for the one window that opened");
    assert_eq!(hints[0].0, handles.window.raw(), "relative to the source");
    assert_eq!(hints[0].1, xui_explorer::shell::open_tile_px(96));
}

#[test]
fn opening_a_file_gives_no_origin_hint() {
    let launcher = Rc::new(TestLauncher::default());
    let (_, _) = drive(
        mem(),
        Rc::clone(&launcher) as Rc<dyn Launcher>,
        "/a",
        |stage, handles| {
            handles.view.set_selection(&[1]);
            stage.emit(Msg::Activate(1)); // top.txt
        },
    );
    assert!(launcher.hints.borrow().is_empty());
}

#[test]
fn the_open_tile_scales_with_dpi() {
    assert_eq!(xui_explorer::shell::open_tile_px(96), 64);
    assert_eq!(xui_explorer::shell::open_tile_px(192), 128);
}

#[test]
fn an_unreadable_folder_shows_the_error_and_an_empty_view() {
    let platform = Rc::new(MemPlatform::new().dir("/a").unreadable("/a"));
    let (_, handles) = drive(platform, Rc::new(TestLauncher::default()), "/a", |_, _| {});
    assert_eq!(handles.view.len(), 0);
    assert!(has(handles.status.text(0), "permission"));
}

#[test]
fn a_refresh_remaps_the_selection_by_name() {
    let platform = mem();
    let mutator = Rc::clone(&platform);
    let (_, handles) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            // Entries are folders first: b, then top.txt.
            handles.view.set_selection(&[0, 1]);
            mutator.remove(Path::new("/a/b"), true).expect("remove");
            stage.emit(Msg::Refresh);
        },
    );
    assert_eq!(
        handles.view.selection(),
        vec![0],
        "top.txt keeps its selection"
    );
}

#[test]
fn a_successful_delete_refreshes_and_closes_descendant_windows() {
    let platform = mem();
    let keeper = Rc::clone(&platform);
    let (explorer, _handles) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        |stage, handles| {
            handles.view.set_selection(&[0]);
            stage.emit(Msg::Activate(0)); // open /a/b in its own window
            stage.emit(Msg::Delete);
            stage.emit(Msg::Confirm(TaskDialogAction::Command(0)));
        },
    );
    assert!(
        !explorer.registry().is_open(Path::new("/a/b")),
        "the deleted folder's window closed"
    );
    assert_eq!(keeper.children("/a"), vec![OsString::from("top.txt")]);
}

#[test]
fn a_pending_confirm_re_resolves_an_item_that_changed() {
    let mem = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .file("/a/a.txt", 1)
            .file("/a/b.txt", 2),
    );
    let platform: Rc<dyn Platform> = mem.clone();
    let mutator = Rc::clone(&mem);
    let (_, _) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            handles.view.set_selection(&[0]); // a.txt
            stage.emit(Msg::Delete); // pending refers to a.txt by name
            mutator
                .remove(Path::new("/a/a.txt"), false)
                .expect("vanish");
            stage.emit(Msg::Confirm(TaskDialogAction::Command(0)));
        },
    );
    // The item was already gone; only b.txt is left and nothing panicked.
    assert_eq!(mem.children("/a"), vec![OsString::from("b.txt")]);
}

#[test]
fn deleting_a_symlink_removes_only_the_link() {
    let mem = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .file("/a/target", 5)
            .symlink("/a/link"),
    );
    let platform: Rc<dyn Platform> = mem.clone();
    let keeper = Rc::clone(&mem);
    let (_, _) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        |stage, handles| {
            handles.view.set_selection(&[0]); // "link" sorts before "target"
            stage.emit(Msg::Delete);
            stage.emit(Msg::Confirm(TaskDialogAction::Command(0)));
        },
    );
    assert_eq!(keeper.children("/a"), vec![OsString::from("target")]);
}

#[test]
fn a_partial_failure_is_reported_and_the_view_reflects_the_disk() {
    let mem = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .file("/a/a.txt", 1)
            .file("/a/b.txt", 2)
            .undeletable("/a/a.txt"),
    );
    let platform: Rc<dyn Platform> = mem.clone();
    let keeper = Rc::clone(&mem);
    let (_, handles) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        |stage, handles| {
            handles.view.set_selection(&[0, 1]);
            stage.emit(Msg::Delete);
            stage.emit(Msg::Confirm(TaskDialogAction::Command(0)));
        },
    );
    assert_eq!(
        keeper.children("/a"),
        vec![OsString::from("a.txt")],
        "only b.txt went"
    );
    assert!(has(handles.status.text(0), "Could not delete"));
    assert!(has(handles.status.text(0), "a.txt"));
}

#[test]
fn activating_a_file_asks_the_launcher() {
    let platform: Rc<dyn Platform> = Rc::new(MemPlatform::new().dir("/a").file("/a/note.txt", 1));
    let launcher = Rc::new(TestLauncher::default());
    let keeper = Rc::clone(&launcher);
    let (_, _) = drive(platform, launcher, "/a", |stage, _| {
        stage.emit(Msg::Activate(0));
    });
    assert_eq!(
        keeper.opened.borrow().as_slice(),
        [PathBuf::from("/a/note.txt")]
    );
}

#[test]
fn a_launcher_error_goes_to_the_status_bar() {
    let platform: Rc<dyn Platform> = Rc::new(MemPlatform::new().dir("/a").file("/a/note.txt", 1));
    let (_, handles) = drive(
        platform,
        Rc::new(TestLauncher::failing()),
        "/a",
        |stage, _| {
            stage.emit(Msg::Activate(0));
        },
    );
    assert!(has(handles.status.text(0), "Cannot open"));
    assert!(has(handles.status.text(0), "note.txt"));
}

#[test]
fn the_title_is_the_folder_name_and_survives_a_refresh() {
    let platform: Rc<dyn Platform> = Rc::new(MemPlatform::new().dir("/parent/docs"));
    let (explorer, handles) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/parent/docs",
        |stage, _| {
            stage.emit(Msg::Refresh);
        },
    );
    assert_eq!(explorer.title_of(handles.window).as_deref(), Some("docs"));
}
