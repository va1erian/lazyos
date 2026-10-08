#![forbid(unsafe_code)]

//! The shared shell: the platform, the launcher, the session and what every
//! open window shows.
//!
//! [`Explorer`] owns a `Rc<dyn Platform>`, a `Rc<dyn Launcher>` and a
//! `Rc<dyn Session>`, and keeps one [`ViewState`] per open window (its folder,
//! its active view, its selection and a [`Proxy`](xui_core::app::Proxy) to
//! reach it). A window navigates in place, so the shell never maps a folder
//! to "its" window: after a delete or a paste it asks every window showing
//! the folder, or a folder below it, to refresh, and a window whose folder
//! is gone climbs to the nearest folder that still exists.

mod views;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{PlatformSpec, WidgetId, WindowId};
use xui_core::units::Dip;

use crate::model::{is_within, title};
use crate::platform::{Launcher, NoSession, Platform, Session};
use crate::window::{ExplorerWindow, Msg, ViewOptions};

pub use views::{ViewState, Views};

/// The width each new window opens at.
const WINDOW_WIDTH: Dip = Dip(720.0);
/// The height each new window opens at.
const WINDOW_HEIGHT: Dip = Dip(480.0);

/// The open animation's start tile, in design pixels (an icon and its label).
const OPEN_TILE_DIP: f32 = 64.0;

/// Answers which widget has the keyboard focus, when the backend knows.
type FocusProbe = Box<dyn Fn() -> Option<WidgetId>>;

/// The shared bits behind every explorer window.
pub struct Explorer {
    platform: Rc<dyn Platform>,
    launcher: Rc<dyn Launcher>,
    session: Rc<dyn Session>,
    /// The title last published by each window, keyed by its window id. Used by
    /// tests to read a title the portable backend does not offer back.
    titles: RefCell<HashMap<u64, String>>,
    /// What each window shows: for drag and drop, refreshes and tests.
    views: Views,
    focus: RefCell<Option<FocusProbe>>,
}

impl Explorer {
    /// A shell over `platform` and `launcher`.
    pub fn new(platform: Rc<dyn Platform>, launcher: Rc<dyn Launcher>) -> Rc<Explorer> {
        Explorer::with_session(platform, launcher, Rc::new(NoSession))
    }

    /// A shell over `platform` and `launcher` that copies, pastes and
    /// announces its selection through `session`.
    pub fn with_session(
        platform: Rc<dyn Platform>,
        launcher: Rc<dyn Launcher>,
        session: Rc<dyn Session>,
    ) -> Rc<Explorer> {
        Rc::new(Explorer {
            platform,
            launcher,
            session,
            titles: RefCell::new(HashMap::new()),
            views: Views::default(),
            focus: RefCell::new(None),
        })
    }

    /// Tells the windows which widget has the keyboard focus, so a shortcut
    /// such as Delete or Backspace is left to the address bar while the user
    /// types in it. Without a probe a window assumes the address bar has the
    /// focus only while it holds an unsubmitted edit.
    pub fn set_focus_probe(&self, probe: impl Fn() -> Option<WidgetId> + 'static) {
        *self.focus.borrow_mut() = Some(Box::new(probe));
    }

    /// Whether `widget` has the focus: `None` when there is no probe.
    pub(crate) fn has_focus(&self, widget: WidgetId) -> Option<bool> {
        self.focus
            .borrow()
            .as_ref()
            .map(|probe| probe() == Some(widget))
    }

    /// The filesystem.
    pub fn platform(&self) -> &dyn Platform {
        self.platform.as_ref()
    }

    /// The launcher.
    pub fn launcher(&self) -> &dyn Launcher {
        self.launcher.as_ref()
    }

    /// The clipboard and the selection feed.
    pub fn session(&self) -> &dyn Session {
        self.session.as_ref()
    }

    /// The user's home directory, when the platform has one.
    pub fn home(&self) -> Option<PathBuf> {
        self.platform.home()
    }

    /// Builds the first window, showing `path`.
    pub fn open_root(self: &Rc<Self>, ui: &mut Ui<Msg>, path: PathBuf) -> ExplorerWindow {
        ExplorerWindow::new(ui, Rc::clone(self), path).expect("the explorer's widgets built")
    }

    /// [`open_root`](Self::open_root) with the entry `name` selected: a
    /// "reveal" of one item in its folder. Also returns whether the folder
    /// held it (when not, the selection is left as a plain open leaves it).
    pub fn reveal_root(
        self: &Rc<Self>,
        ui: &mut Ui<Msg>,
        dir: PathBuf,
        name: &OsStr,
    ) -> (ExplorerWindow, bool) {
        let mut window = self.open_root(ui, dir);
        let found = window.select_name(name);
        (window, found)
    }

    /// Opens `path` in a new window that starts with `options` (the opener's
    /// view and sort). Returns whether the window opened.
    pub fn open_window(self: &Rc<Self>, ui: &Ui<Msg>, path: PathBuf, options: ViewOptions) -> bool {
        self.launcher
            .hint_open_origin(ui.window().raw(), open_tile_px(ui.dpi()));
        let shell = Rc::clone(self);
        let spec = PlatformSpec::new(title(&path)).size(WINDOW_WIDTH, WINDOW_HEIGHT);
        // `open_window` runs the child's build (and drains its first
        // messages) synchronously; nothing of the shell is borrowed across it.
        ui.open_window(spec, move |ui| {
            ExplorerWindow::with_options(ui, shell, path, options)
                .expect("the explorer's widgets built")
        })
        .is_ok()
    }

    /// Sends [`Msg::Refresh`] to every window other than `except` that shows
    /// `dir` or a folder below it (a folder that was deleted makes its window
    /// climb to the nearest folder still there).
    pub fn refresh_under(&self, dir: &Path, except: Option<WindowId>) {
        for state in self.views.all() {
            let skipped = except.is_some_and(|except| except.raw() == state.window);
            if !skipped && is_within(&state.dir, dir) {
                let _ = state.proxy.send(Msg::Refresh);
            }
        }
    }

    /// Sends [`Msg::Refresh`] to every open window (after a drop that may
    /// have moved items out of any of them).
    pub fn refresh_all(&self) {
        for state in self.views.all() {
            let _ = state.proxy.send(Msg::Refresh);
        }
    }

    /// The folder every open window shows, in no particular order.
    pub fn open_dirs(&self) -> Vec<PathBuf> {
        self.views
            .all()
            .into_iter()
            .map(|state| state.dir)
            .collect()
    }

    /// What window `window` (a raw window id) shows, while it is open.
    pub fn view_state(&self, window: u64) -> Option<ViewState> {
        self.views.get(window)
    }

    /// Records what a window shows.
    pub(crate) fn publish_view(&self, state: ViewState) {
        self.views.publish(state);
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
