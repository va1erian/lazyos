#![forbid(unsafe_code)]

//! The shared shell: the platform, the launcher and the window registry.
//!
//! [`Explorer`] owns a `Rc<dyn Platform>` and a `Rc<dyn Launcher>` and keeps
//! one entry per open folder window, so opening a folder that is already shown
//! is a no-op and deleting a folder can close the windows below it. There is no
//! raise/focus API in the portable layer, so a duplicate open reports
//! `"already open"` in the status bar instead of raising the existing window.
//!
//! The registry never holds its borrow across a call that can call back
//! (`open_window`, `close`, `send`): it collects what it needs, drops the
//! borrow, then acts. See [`registry`] for the map and its tests.

mod registry;
mod views;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::app::{Ui, WindowHandle};
use xui_core::backend::{PlatformSpec, WindowId};
use xui_core::units::Dip;

use crate::model::title;
use crate::platform::{Launcher, Platform};
use crate::window::{ExplorerWindow, Msg};

pub use registry::{Closable, Registry};
pub use views::{ViewState, Views};

/// The width each folder window opens at.
const WINDOW_WIDTH: Dip = Dip(720.0);
/// The height each folder window opens at.
const WINDOW_HEIGHT: Dip = Dip(480.0);

/// The open animation's start tile, in design pixels (an icon and its label).
const OPEN_TILE_DIP: f32 = 64.0;

/// The shared bits behind every explorer window.
pub struct Explorer {
    platform: Rc<dyn Platform>,
    launcher: Rc<dyn Launcher>,
    registry: Registry<WindowHandle<Msg>>,
    /// The title last published by each window, keyed by its window id. Used by
    /// tests to read a title the portable backend does not offer back.
    titles: RefCell<HashMap<u64, String>>,
    /// What each window shows, for the platform's drag and drop.
    views: Views,
}

impl Explorer {
    /// A shell over `platform` and `launcher`.
    pub fn new(platform: Rc<dyn Platform>, launcher: Rc<dyn Launcher>) -> Rc<Explorer> {
        Rc::new(Explorer {
            platform,
            launcher,
            registry: Registry::new(),
            titles: RefCell::new(HashMap::new()),
            views: Views::default(),
        })
    }

    /// The filesystem.
    pub fn platform(&self) -> &dyn Platform {
        self.platform.as_ref()
    }

    /// The launcher.
    pub fn launcher(&self) -> &dyn Launcher {
        self.launcher.as_ref()
    }

    /// The user's home directory, when the platform has one.
    pub fn home(&self) -> Option<PathBuf> {
        self.platform.home()
    }

    /// The registry, for a test that checks duplicate/close behaviour.
    pub fn registry(&self) -> &Registry<WindowHandle<Msg>> {
        &self.registry
    }

    /// Builds the primary (root) window for `path` and records it so a later
    /// request to open the same folder is a no-op.
    pub fn open_root(self: &Rc<Self>, ui: &mut Ui<Msg>, path: PathBuf) -> ExplorerWindow {
        self.registry.register_primary(path.clone());
        ExplorerWindow::new(ui, Rc::clone(self), path).expect("the explorer's widgets built")
    }

    /// Opens `path` in its own window unless a window already shows it. Returns
    /// whether a new window was opened.
    pub fn open_or_reuse(self: &Rc<Self>, ui: &Ui<Msg>, path: PathBuf) -> bool {
        if self.registry.is_open(&path) {
            return false;
        }
        self.launcher
            .hint_open_origin(ui.window().raw(), open_tile_px(ui.dpi()));
        let shell = Rc::clone(self);
        let child_path = path.clone();
        let spec = window_spec(&path);
        // `open_window` is called with no registry borrow held. It runs the
        // child's build (and drains its first messages) synchronously.
        match ui.open_window(spec, move |ui| {
            ExplorerWindow::new(ui, shell, child_path).expect("the explorer's widgets built")
        }) {
            Ok(handle) => {
                self.registry.register(path, handle);
                true
            }
            Err(_) => false,
        }
    }

    /// Closes every secondary window showing `dir` or a folder below it.
    pub fn close_under(&self, dir: &Path) {
        self.registry.close_under(dir);
    }

    /// Sends [`Msg::Refresh`] to every other window showing `dir`.
    pub fn refresh_windows_showing(&self, dir: &Path, except: WindowId) {
        let handles = self.registry.handles_at(dir);
        for handle in handles {
            if handle.window() != except {
                handle.send(Msg::Refresh);
            }
        }
    }

    /// What window `window` (a raw window id) shows, while it is open.
    pub fn view_state(&self, window: u64) -> Option<ViewState> {
        self.views.get(window)
    }

    /// Records what a window shows.
    pub(crate) fn publish_view(&self, window: u64, state: ViewState) {
        self.views.publish(window, state);
    }

    /// Forgets a closed window.
    pub(crate) fn forget_view(&self, window: u64) {
        self.views.forget(window);
    }

    /// Records the title a window published.
    pub fn publish_title(&self, window: WindowId, title: &str) {
        self.titles
            .borrow_mut()
            .insert(window.raw(), title.to_string());
    }

    /// The last title a window published, if it has published one.
    pub fn title_of(&self, window: WindowId) -> Option<String> {
        self.titles.borrow().get(&window.raw()).cloned()
    }
}

/// The edge, in device pixels, of the tile the open animation starts from:
/// about an icon plus its label, so the wireframe leaves the whole tile.
pub fn open_tile_px(dpi: u32) -> i32 {
    Dip(OPEN_TILE_DIP).to_px(dpi).value()
}

/// The [`PlatformSpec`] a folder window opens with.
fn window_spec(path: &Path) -> PlatformSpec {
    PlatformSpec::new(title(path)).size(WINDOW_WIDTH, WINDOW_HEIGHT)
}
