//! The state the shell's three surfaces share: the models, the theme, the
//! window handles and the compositor connection.
//!
//! The desktop, the taskbar and the start menu are three xui windows, each an
//! `App` with its own message type, driven by one backend loop on one thread;
//! they share this context through an `Rc`, and every mutable part sits in a
//! `RefCell`/`Cell` that is never borrowed across a call into another window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lazyshell::menu::Menu;
use lazyshell::taskbar::{self, Taskbar, BAR_H};
use lazyshell::{Entry, Rect};
use xui_core::app::WindowHandle;

use super::menu::MenuMsg;
use super::services;
use super::taskbar::BarMsg;
use super::theme::ThemeFeed;
use crate::backend::LazyOSBackend;
use crate::display::Client;

/// What the pointer is over on the taskbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarHover {
    Start,
    Entry(usize),
}

/// The shared shell state.
pub struct Ctx {
    pub backend: Rc<LazyOSBackend>,
    /// The compositor connection (the backend's own).
    pub client: Client,
    /// Screen size in pixels.
    pub screen: (i32, i32),
    /// This shell's kernel-stamped uid (the service's caller rule).
    pub uid: u32,
    /// This task's end of the `shell` subscription's event channel.
    pub events: u64,
    pub taskbar: RefCell<Taskbar>,
    /// Each taskbar entry's panel-local rectangle (`None` = hidden).
    pub entries: RefCell<Vec<Option<Rect>>>,
    /// The icon geometry last sent per surface, so only changes are sent.
    icon_sent: RefCell<Vec<(u64, Rect)>>,
    /// The width the clock reserves (text plus padding is added later).
    pub clock_w: Cell<i32>,
    pub clock: RefCell<String>,
    pub bar_hover: Cell<Option<BarHover>>,
    pub menu: RefCell<Menu>,
    pub menu_hover: Cell<Option<usize>>,
    /// The desktop launchers (`sys/ui/desktop`).
    pub launchers: RefCell<Vec<Entry>>,
    /// Set when `launchers` changed and the desktop must rebuild its view.
    pub launchers_changed: Cell<bool>,
    pub theme: RefCell<ThemeFeed>,
    pub bar: RefCell<Option<WindowHandle<BarMsg>>>,
    pub menu_window: RefCell<Option<WindowHandle<MenuMsg>>>,
    /// Keys of the one-time log lines already printed.
    noted: RefCell<Vec<&'static str>>,
}

impl Ctx {
    pub fn new(
        backend: Rc<LazyOSBackend>,
        client: Client,
        screen: (i32, i32),
        uid: u32,
        events: u64,
    ) -> Ctx {
        Ctx {
            backend,
            client,
            screen,
            uid,
            events,
            taskbar: RefCell::new(Taskbar::new()),
            entries: RefCell::new(Vec::new()),
            icon_sent: RefCell::new(Vec::new()),
            clock_w: Cell::new(0),
            clock: RefCell::new(String::new()),
            bar_hover: Cell::new(None),
            menu: RefCell::new(Menu::default()),
            menu_hover: Cell::new(None),
            launchers: RefCell::new(lazyshell::desktop::defaults()),
            launchers_changed: Cell::new(false),
            theme: RefCell::new(ThemeFeed::new()),
            bar: RefCell::new(None),
            menu_window: RefCell::new(None),
            noted: RefCell::new(Vec::new()),
        }
    }

    /// Print `line` the first time `key` is noted, so a failure that repeats
    /// every tick (an older compositor answering `EINVAL`) logs once.
    pub fn note(&self, key: &'static str, line: impl FnOnce() -> String) {
        if !self.noted.borrow().contains(&key) {
            self.noted.borrow_mut().push(key);
            println!("{}", line());
        }
    }

    /// The taskbar panel's screen `y`.
    pub fn bar_y(&self) -> i32 {
        self.screen.1 - BAR_H
    }

    /// The clock's panel-local rectangle.
    pub fn clock_rect(&self) -> Rect {
        taskbar::clock_rect(self.screen.0, self.clock_w.get())
    }

    /// Ask the taskbar to repaint.
    pub fn repaint_bar(&self) {
        if let Some(bar) = &*self.bar.borrow() {
            bar.send(BarMsg::Repaint);
        }
    }

    /// Ask the open start menu to repaint.
    pub fn repaint_menu(&self) {
        if let Some(menu) = &*self.menu_window.borrow() {
            menu.send(MenuMsg::Repaint);
        }
    }

    /// The window list changed: lay the entries out again, tell the
    /// compositor where each entry now is (only what moved), and repaint.
    pub fn bar_changed(&self) {
        let count = self.taskbar.borrow().windows().len();
        let rects = taskbar::entry_rects(count, self.screen.0, self.clock_rect().w);
        let surfaces: Vec<u64> = self
            .taskbar
            .borrow()
            .windows()
            .iter()
            .map(|window| window.surface)
            .collect();
        let mut next = Vec::with_capacity(count);
        for (surface, rect) in surfaces.iter().zip(&rects) {
            // A hidden entry sends an empty rectangle: the compositor then
            // zooms to its bottom-left default instead of a stale spot.
            let screen = rect.map_or(Rect::default(), |r| r.offset(0, self.bar_y()));
            next.push((*surface, screen));
        }
        let changed: Vec<(u64, Rect)> = next
            .iter()
            .filter(|entry| !self.icon_sent.borrow().contains(entry))
            .copied()
            .collect();
        for (surface, rect) in changed {
            if let Err(code) = self
                .client
                .set_icon_geometry(surface, (rect.x, rect.y, rect.w, rect.h))
            {
                self.note("icon", || format!("SHELL:ICONGEOM:FAIL err={}", -code));
            }
        }
        *self.icon_sent.borrow_mut() = next;
        *self.entries.borrow_mut() = rects;
        self.bar_hover.set(None);
        self.repaint_bar();
    }

    /// Launch `app` through `init`, after asking the compositor to zoom the
    /// new window open from `origin` (a screen rectangle). Prints the
    /// `SHELL:LAUNCH` marker and returns the pid or the negative errno.
    pub fn launch(&self, app: &str, origin: Option<Rect>) -> Result<u64, i64> {
        if let Some(rect) = origin.filter(|rect| !rect.is_empty()) {
            let hint = (rect.x, rect.y, rect.w as u32, rect.h as u32);
            if let Err(code) = self.client.hint_launch_origin(hint) {
                self.note("hint", || format!("SHELL:HINT:FAIL err={}", -code));
            }
        }
        let result = services::launch(app);
        match result {
            Ok(pid) => println!("SHELL:LAUNCH:PASS app={app} pid={pid}"),
            Err(code) => println!("SHELL:LAUNCH:FAIL app={app} err={}", -code),
        }
        result
    }

    /// Re-read the start menu: `sys/ui/menu` plus `init`'s installed apps.
    pub fn reload_menu(&self) {
        let stored = services::confd_get(deskmenu::KEY).ok().flatten();
        let configured = deskmenu::from_value(stored.as_ref(), &|_| true);
        let apps = services::list_apps();
        let (installed, ids) = match &apps {
            Ok(apps) => (
                lazyshell::menu::installed_entries(
                    apps.iter()
                        .map(|app| (app.id.as_str(), app.name.as_str(), app.installed)),
                ),
                Some(apps.iter().map(|app| app.id.clone()).collect::<Vec<_>>()),
            ),
            Err(code) => {
                self.note("list-apps", || {
                    format!("SHELL:MENU:APPS:FAIL err={}", -code)
                });
                (Vec::new(), None)
            }
        };
        let shipped = match &ids {
            Some(ids) => lazyshell::menu::Shipped::Known(ids),
            None => lazyshell::menu::Shipped::Unknown,
        };
        *self.menu.borrow_mut() = Menu::build(&installed, &configured, shipped, self.screen.1);
        self.menu_hover.set(None);
    }

    /// Re-read the desktop launchers; `true` when they changed.
    pub fn reload_launchers(&self) -> bool {
        let Ok(stored) = services::confd_get(lazyshell::desktop::KEY) else {
            return false;
        };
        let next = lazyshell::desktop::from_value(stored.as_ref());
        if *self.launchers.borrow() == next {
            return false;
        }
        *self.launchers.borrow_mut() = next;
        self.launchers_changed.set(true);
        true
    }
}
