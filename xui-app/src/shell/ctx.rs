//! The state the shell's three surfaces share: the models, the theme, the
//! window handles and the compositor connection.
//!
//! The desktop, the taskbar and the start menu are three xui windows, each an
//! `App` with its own message type, driven by one backend loop on one thread;
//! they share this context through an `Rc`, and every mutable part sits in a
//! `RefCell`/`Cell` that is never borrowed across a call into another window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lazyshell::menu::{listed_hidden, visible, Listed, Menu};
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
    pub uid: Option<u32>,
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
    /// The desktop launchers on screen: `sys/ui/desktop` minus the apps this
    /// user hides.
    pub launchers: RefCell<Vec<Entry>>,
    /// Each launcher's package icon path (`ListApps.icon`; empty: draw the
    /// built-in picture), parallel to `launchers`.
    pub launcher_icons: RefCell<Vec<String>>,
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
        uid: Option<u32>,
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
            launcher_icons: RefCell::new(Vec::new()),
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

    /// The desktop's UI scale: every shell geometry is in design pixels,
    /// and the protocol and the canvas take screen pixels.
    pub fn scale(&self) -> i32 {
        self.backend.scale() as i32
    }

    /// A design-pixel rectangle in screen pixels.
    pub fn to_screen(&self, rect: Rect) -> Rect {
        let s = self.scale();
        Rect::new(rect.x * s, rect.y * s, rect.w * s, rect.h * s)
    }

    /// A screen-pixel point in design pixels.
    pub fn to_design(&self, x: i32, y: i32) -> (i32, i32) {
        let s = self.scale();
        (x.div_euclid(s), y.div_euclid(s))
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
            let rect = self.to_screen(rect);
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
            let rect = self.to_screen(rect);
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

    /// Re-read the start menu: `sys/ui/menu` plus `init`'s installed apps,
    /// minus the apps `ListApps` marks hidden for this user.
    pub fn reload_menu(&self) {
        let stored = services::confd_get(deskmenu::KEY).ok().flatten();
        let apps = services::list_apps();
        let listed: Vec<Listed<'_>> = match &apps {
            Ok(apps) => apps.iter().map(services::App::listed).collect(),
            Err(code) => {
                self.note("list-apps", || {
                    format!("SHELL:MENU:APPS:FAIL err={}", -code)
                });
                Vec::new()
            }
        };
        let configured = visible(deskmenu::from_value(stored.as_ref(), &|_| true), |app| {
            listed_hidden(&listed, app)
        });
        let installed = lazyshell::menu::installed_entries(listed.iter().copied());
        let ids: Option<Vec<String>> = apps
            .as_ref()
            .ok()
            .map(|apps| apps.iter().map(|app| app.id.clone()).collect());
        let shipped = match &ids {
            Some(ids) => lazyshell::menu::Shipped::Known(ids),
            None => lazyshell::menu::Shipped::Unknown,
        };
        *self.menu.borrow_mut() = Menu::build(&installed, &configured, shipped, self.screen.1);
        self.menu_hover.set(None);
    }

    /// Re-read the desktop launchers (`sys/ui/desktop`, then `ListApps` for
    /// what this user hides and each package's icon); `true` when they
    /// changed. An unreachable `init` hides nothing and draws the built-in
    /// pictures.
    pub fn reload_launchers(&self) -> bool {
        let Ok(stored) = services::confd_get(lazyshell::desktop::KEY) else {
            return false;
        };
        let apps = services::list_apps().unwrap_or_default();
        let listed: Vec<Listed<'_>> = apps.iter().map(services::App::listed).collect();
        let next = visible(lazyshell::desktop::from_value(stored.as_ref()), |app| {
            listed_hidden(&listed, app)
        });
        let icons: Vec<String> = next
            .iter()
            .map(|entry| {
                apps.iter()
                    .find(|app| deskmenu::same_app(&app.id, &entry.app))
                    .map(|app| app.icon.clone())
                    .unwrap_or_default()
            })
            .collect();
        if *self.launchers.borrow() == next && *self.launcher_icons.borrow() == icons {
            return false;
        }
        println!("SHELL:DESKTOP:ICONS n={}", next.len());
        *self.launchers.borrow_mut() = next;
        *self.launcher_icons.borrow_mut() = icons;
        self.launchers_changed.set(true);
        true
    }
}
