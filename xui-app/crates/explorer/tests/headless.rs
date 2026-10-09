#![forbid(unsafe_code)]

//! Headless UI tests: a window over an in-memory filesystem, driven through the
//! offscreen backend's synthetic input and messages. No real window opens and
//! the loop runs to completion synchronously, so there is no watchdog to arm.
//!
//! This file holds opening files and deleting; `headless/nav.rs` the
//! in-place navigation and `headless/views.rs` the icon and details views.

#[path = "headless/harness.rs"]
mod harness;
#[path = "headless/nav.rs"]
mod nav;
#[path = "headless/views.rs"]
mod views;

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::TaskDialogAction;
use xui_explorer::MemPlatform;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::window::Msg;

use harness::{TestLauncher, drive, has, mem, slash};

#[test]
fn an_unreadable_start_folder_shows_the_error_and_an_empty_view() {
    let platform = Rc::new(MemPlatform::new().dir("/a").unreadable("/a"));
    let (_, handles) = drive(platform, Rc::new(TestLauncher::default()), "/a", |_, _| {});
    assert_eq!(handles.icons.len(), 0);
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
            handles.icons.set_selection(&[0, 1]);
            mutator.remove(Path::new("/a/b"), true).expect("remove");
            stage.emit(Msg::Refresh);
        },
    );
    assert_eq!(
        handles.icons.selection(),
        vec![0],
        "top.txt keeps its selection"
    );
}

/// The second window's own refresh (it climbs out of the deleted folder) is
/// `nav::a_refresh_inside_a_deleted_folder_climbs_to_the_nearest_folder`:
/// the offscreen stage pumps only the first window's queue.
#[test]
fn a_folder_opens_in_a_new_window_and_can_be_deleted_from_its_parent() {
    let platform = mem();
    let keeper = Rc::clone(&platform);
    let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
    let log = Rc::clone(&seen);
    drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            stage.emit(Msg::OpenInNewWindow(0)); // /a/b in a second window
            let mut dirs: Vec<String> = handles
                .explorer
                .open_dirs()
                .iter()
                .map(|dir| slash(dir))
                .collect();
            dirs.sort();
            log.borrow_mut().push(dirs);
            handles.icons.set_selection(&[0]);
            stage.emit(Msg::Delete);
            stage.emit(Msg::Confirm(TaskDialogAction::Command(0)));
            log.borrow_mut().push(vec![slash(&handles.dir())]);
        },
    );
    assert_eq!(*seen.borrow(), [vec!["/a", "/a/b"], vec!["/a"]]);
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
            handles.icons.set_selection(&[0]); // a.txt
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
            handles.icons.set_selection(&[0]); // "link" sorts before "target"
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
            handles.icons.set_selection(&[0, 1]);
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
fn opening_a_new_window_hints_its_origin_and_a_file_does_not() {
    let launcher = Rc::new(TestLauncher::default());
    let (_, handles) = drive(
        mem(),
        Rc::clone(&launcher) as Rc<dyn Launcher>,
        "/a",
        |stage, _| {
            stage.emit(Msg::OpenInNewWindow(0)); // the folder b
            stage.emit(Msg::OpenInNewWindow(1)); // top.txt: not a folder
            stage.emit(Msg::Activate(0)); // in place: no animation
        },
    );
    let hints = launcher.hints.borrow();
    assert_eq!(hints.len(), 1, "one hint for the one window that opened");
    assert_eq!(hints[0].0, handles.window.raw(), "relative to the source");
    assert_eq!(hints[0].1, xui_explorer::shell::open_tile_px(96));
}

#[test]
fn an_unreadable_folder_does_not_open_a_window() {
    let platform = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/locked")
            .unreadable("/a/locked"),
    );
    let launcher = Rc::new(TestLauncher::default());
    let (_, handles) = drive(
        platform,
        Rc::clone(&launcher) as Rc<dyn Launcher>,
        "/a",
        |stage, _| stage.emit(Msg::OpenInNewWindow(0)),
    );
    assert!(launcher.hints.borrow().is_empty(), "no window opened");
    assert!(has(handles.status.text(0), "Cannot open locked"));
}

#[test]
fn the_open_tile_scales_with_dpi() {
    assert_eq!(xui_explorer::shell::open_tile_px(96), 64);
    assert_eq!(xui_explorer::shell::open_tile_px(192), 128);
}
