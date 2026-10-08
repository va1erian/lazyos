//! The harness the headless tests share: a recording launcher and one
//! explorer window rendered offscreen over a `MemPlatform`.

use std::cell::RefCell;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::backend::{BackendError, WindowId};
use xui_core::units::Dip;
use xui_core::widget::{Edit, IconView, ListView, StatusBar};
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::window::Msg;
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
            ..TestLauncher::default()
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
    pub explorer: Rc<Explorer>,
    pub icons: Rc<IconView<Msg>>,
    pub details: Rc<ListView<Msg>>,
    pub address: Rc<Edit<Msg>>,
    pub status: Rc<StatusBar<Msg>>,
    pub window: WindowId,
    pub sort_popup: Option<xui_core::backend::WidgetId>,
}

impl Handles {
    /// The folder the window shows now.
    pub fn dir(&self) -> PathBuf {
        self.explorer
            .view_state(self.window.raw())
            .expect("the window is open")
            .dir
    }

    /// The window's title now.
    pub fn title(&self) -> String {
        self.explorer.title_of(self.window).unwrap_or_default()
    }

    /// The selection the window last published, as paths.
    pub fn selected(&self) -> Vec<PathBuf> {
        self.explorer
            .view_state(self.window.raw())
            .expect("the window is open")
            .selected
    }
}

/// `path` with `/` separators, so an expectation reads the same on Windows.
pub fn slash(path: &Path) -> String {
    slashed(&path.display().to_string())
}

/// `text` with `\` turned into `/`.
pub fn slashed(text: &str) -> String {
    text.replace('\\', "/")
}

pub fn has(part: Option<String>, needle: &str) -> bool {
    part.map(|part| part.contains(needle)).unwrap_or(false)
}

/// Builds one explorer window over `platform` showing `dir` and runs `step`
/// before the capture. Returns the shell and the handles.
pub fn drive<F>(
    platform: Rc<dyn Platform>,
    launcher: Rc<dyn Launcher>,
    dir: &str,
    step: F,
) -> (Rc<Explorer>, Handles)
where
    F: FnOnce(&Stage<'_, Msg>, &Handles) + 'static,
{
    let explorer = Explorer::new(platform, launcher);
    let slot: Rc<RefCell<Option<Handles>>> = Rc::new(RefCell::new(None));
    let slot_build = Rc::clone(&slot);
    let slot_step = Rc::clone(&slot);
    let explorer_build = Rc::clone(&explorer);
    let dir = dir.to_string();
    render_with(
        Snapshot::new(Dip(560.0), Dip(360.0)),
        move |ui| {
            let window = ExplorerWindow::new(ui, Rc::clone(&explorer_build), PathBuf::from(&dir))?;
            *slot_build.borrow_mut() = Some(Handles {
                explorer: Rc::clone(&explorer_build),
                icons: window.view_handle(),
                details: window.details_handle(),
                address: window.address_handle(),
                status: window.status_bar(),
                window: ui.window(),
                sort_popup: window.sort_menu_popup(),
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
    (explorer, handles)
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
