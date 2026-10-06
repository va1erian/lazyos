//! The harness the headless tests share: a recording launcher and one
//! explorer window rendered offscreen over a `MemPlatform`.

use std::cell::{Cell, RefCell};
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::backend::{BackendError, WindowId};
use xui_core::units::Dip;
use xui_core::widget::{IconView, StatusBar};
use xui_explorer::model::Clock;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::window::{FlashHandle, Msg};
use xui_explorer::{Explorer, ExplorerWindow, MemPlatform};

/// A launcher that records what it was asked to open, or fails on demand.
#[derive(Default)]
pub struct TestLauncher {
    fail: bool,
    pub opened: RefCell<Vec<PathBuf>>,
    pub hints: RefCell<Vec<(u64, i32)>>,
}

impl TestLauncher {
    pub fn failing() -> TestLauncher {
        TestLauncher {
            fail: true,
            opened: RefCell::new(Vec::new()),
            hints: RefCell::new(Vec::new()),
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

    fn hint_open_origin(&self, window: u64, tile: i32) {
        self.hints.borrow_mut().push((window, tile));
    }
}

/// The handles a test keeps after the app is built.
pub struct Handles {
    pub view: Rc<IconView<Msg>>,
    pub status: Rc<StatusBar<Msg>>,
    pub flash: FlashHandle,
    pub window: WindowId,
}

pub fn has(part: Option<String>, needle: &str) -> bool {
    part.map(|part| part.contains(needle)).unwrap_or(false)
}

/// Builds one explorer window over `platform` on the system clock and runs
/// `step` before the capture.
pub fn drive<F>(
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
pub fn drive_with_clock<F>(
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

pub fn mem() -> Rc<MemPlatform> {
    Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .file("/a/b/inner.txt", 4)
            .file("/a/top.txt", 2),
    )
}

/// A clock a test can advance, shared with the window's flash.
pub fn advanceable_clock() -> (Rc<Cell<Instant>>, Clock) {
    let now = Rc::new(Cell::new(Instant::now()));
    let clock: Clock = {
        let now = Rc::clone(&now);
        Rc::new(move || now.get())
    };
    (now, clock)
}
