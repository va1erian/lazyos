#![forbid(unsafe_code)]

//! Headless UI tests: a window over an in-memory filesystem, driven through the
//! offscreen backend's synthetic input and messages. No real window opens and
//! the loop runs to completion synchronously, so there is no watchdog to arm.

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::backend::{BackendError, WindowId};
use xui_core::units::Dip;
use xui_core::widget::{IconView, StatusBar, TaskDialogAction};
use xui_explorer::model::Clock;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::window::{FlashHandle, Msg};
use xui_explorer::{Explorer, ExplorerWindow, MemPlatform};

/// A launcher that records what it was asked to open, or fails on demand.
#[derive(Default)]
struct TestLauncher {
    fail: bool,
    opened: RefCell<Vec<PathBuf>>,
}

impl TestLauncher {
    fn failing() -> TestLauncher {
        TestLauncher {
            fail: true,
            opened: RefCell::new(Vec::new()),
        }
    }
}

impl Launcher for TestLauncher {
    fn open(&self, path: &Path) -> io::Result<()> {
        if self.fail {
            return Err(Error::new(ErrorKind::PermissionDenied, "no handler"));
        }
        self.opened.borrow_mut().push(path.to_path_buf());
        Ok(())
    }
}

/// The handles a test keeps after the app is built.
struct Handles {
    view: Rc<IconView<Msg>>,
    status: Rc<StatusBar<Msg>>,
    flash: FlashHandle,
    window: WindowId,
}

fn has(part: Option<String>, needle: &str) -> bool {
    part.map(|part| part.contains(needle)).unwrap_or(false)
}

/// Builds one explorer window over `platform` on the system clock and runs
/// `step` before the capture.
fn drive<F>(
    platform: Rc<dyn Platform>,
    launcher: Rc<dyn Launcher>,
    dir: &str,
    step: F,
) -> (Rc<Explorer>, Handles)
where
    F: FnOnce(&Stage<'_, Msg>, &Handles) + 'static,
{
    let clock: Clock = Rc::new(Instant::now);
    drive_with_clock(platform, launcher, dir, clock, step)
}

/// Builds one explorer window over `platform` whose flash reads `clock`, and
/// runs `step` before the capture. Returns the shell (for registry assertions)
/// and the handles.
fn drive_with_clock<F>(
    platform: Rc<dyn Platform>,
    launcher: Rc<dyn Launcher>,
    dir: &str,
    clock: Clock,
    step: F,
) -> (Rc<Explorer>, Handles)
where
    F: FnOnce(&Stage<'_, Msg>, &Handles) + 'static,
{
    let explorer = Explorer::new(platform, launcher);
    let explorer_out = Rc::clone(&explorer);
    let slot: Rc<RefCell<Option<Handles>>> = Rc::new(RefCell::new(None));
    let slot_build = Rc::clone(&slot);
    let slot_step = Rc::clone(&slot);
    let explorer_build = Rc::clone(&explorer);
    let dir = dir.to_string();
    render_with(
        Snapshot::new(Dip(420.0), Dip(320.0)),
        move |ui| {
            let window = ExplorerWindow::with_clock(
                ui,
                Rc::clone(&explorer_build),
                PathBuf::from(&dir),
                clock,
            )?;
            *slot_build.borrow_mut() = Some(Handles {
                view: window.view_handle(),
                status: window.status_bar(),
                flash: window.flash_handle(),
                window: ui.window(),
            });
            Ok::<_, BackendError>(window)
        },
        move |stage| {
            let handles = slot_step.borrow();
            if let Some(handles) = handles.as_ref() {
                step(stage, handles);
            }
        },
    )
    .expect("the headless render");
    let handles = slot.borrow_mut().take().expect("handles");
    (explorer_out, handles)
}

fn mem() -> Rc<MemPlatform> {
    Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .file("/a/b/inner.txt", 4)
            .file("/a/top.txt", 2),
    )
}

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

/// A clock a test can advance, shared with the window's flash.
fn advanceable_clock() -> (Rc<Cell<Instant>>, Clock) {
    let now = Rc::new(Cell::new(Instant::now()));
    let clock: Clock = {
        let now = Rc::clone(&now);
        Rc::new(move || now.get())
    };
    (now, clock)
}

#[test]
fn activating_a_folder_flashes_it_open_until_the_timer_expires() {
    let (now, clock) = advanceable_clock();
    let step_now = Rc::clone(&now);
    let (_, _) = drive_with_clock(
        mem(),
        Rc::new(TestLauncher::default()),
        "/a",
        clock,
        move |stage, handles| {
            // Entries are folders first: b is item 0.
            handles.view.set_selection(&[1]);
            stage.emit(Msg::Activate(0));
            assert!(handles.flash.is_flashing(OsStr::new("b")), "b flashes");
            assert!(handles.flash.timer_running(), "the tick timer started");
            assert_eq!(
                handles.view.selection(),
                vec![1],
                "flashing does not change the selection"
            );

            step_now.set(step_now.get() + Duration::from_millis(2_000));
            stage.emit(Msg::FlashTick);
            assert!(
                !handles.flash.is_flashing(OsStr::new("b")),
                "b reverted at the deadline"
            );
            assert!(handles.flash.is_empty());
            assert!(
                !handles.flash.timer_running(),
                "the tick timer stopped when the list emptied"
            );
        },
    );
}

#[test]
fn re_activating_a_folder_restarts_its_flash() {
    let (now, clock) = advanceable_clock();
    let step_now = Rc::clone(&now);
    let (_, _) = drive_with_clock(
        mem(),
        Rc::new(TestLauncher::default()),
        "/a",
        clock,
        move |stage, handles| {
            stage.emit(Msg::Activate(0));
            step_now.set(step_now.get() + Duration::from_millis(1_500));
            stage.emit(Msg::Activate(0));
            step_now.set(step_now.get() + Duration::from_millis(1_500));
            assert!(
                handles.flash.is_flashing(OsStr::new("b")),
                "1000 ms since the restart, not yet expired"
            );
            assert_eq!(handles.flash.len(), 1, "no duplicate entry");

            step_now.set(step_now.get() + Duration::from_millis(500));
            stage.emit(Msg::FlashTick);
            assert!(!handles.flash.is_flashing(OsStr::new("b")));
        },
    );
}

#[test]
fn several_folders_flash_at_once_under_one_timer() {
    let platform = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .dir("/a/c")
            .file("/a/z.txt", 1),
    );
    let (now, clock) = advanceable_clock();
    let step_now = Rc::clone(&now);
    let (_, _) = drive_with_clock(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        clock,
        move |stage, handles| {
            stage.emit(Msg::Activate(0)); // b
            stage.emit(Msg::Activate(1)); // c
            assert!(handles.flash.is_flashing(OsStr::new("b")));
            assert!(handles.flash.is_flashing(OsStr::new("c")));
            assert_eq!(handles.flash.len(), 2);
            assert!(handles.flash.timer_running(), "one timer serves both");

            step_now.set(step_now.get() + Duration::from_millis(2_000));
            stage.emit(Msg::FlashTick);
            assert!(handles.flash.is_empty());
            assert!(!handles.flash.timer_running());
        },
    );
}

#[test]
fn deleting_a_flashing_folder_drops_its_flash_on_refresh() {
    let mem = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .file("/a/top.txt", 1),
    );
    let platform: Rc<dyn Platform> = mem.clone();
    let remover = Rc::clone(&mem);
    let (_, _) = drive(
        platform,
        Rc::new(TestLauncher::default()),
        "/a",
        move |stage, handles| {
            stage.emit(Msg::Activate(0)); // b
            assert!(handles.flash.is_flashing(OsStr::new("b")));
            remover.remove(Path::new("/a/b"), true).expect("remove");
            stage.emit(Msg::Refresh);
            assert!(
                !handles.flash.is_flashing(OsStr::new("b")),
                "a folder that vanished stops flashing"
            );
            assert!(handles.flash.is_empty());
            assert!(!handles.flash.timer_running(), "no idle timer is left");
        },
    );
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
