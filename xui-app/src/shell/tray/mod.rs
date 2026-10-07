//! The taskbar tray (docs/tray-plan.md section 7.2): LazyShell serves
//! `os.lazy.shell.tray` and paints one cell per app between the window
//! entries and the clock. The decisions (validation, one item per app, the
//! icon fallback chain, the layout) live in the host-tested
//! `lazyshell::tray`; this module wires them to Messenger and the bar.
//!
//! * [`service`]: register, authorize, identify the caller, apply.
//! * [`liveness`]: `Ping` every item's channel; `EPIPE` drops the item.
//! * [`generation`]: publish `session/<s>/shell/tray` so clients `Set` again
//!   after a shell restart, and report the items that came back.
//! * [`icon`], [`paint`]: resolve and draw each cell's picture, badge and
//!   attention pulse.
//! * [`tooltip`]: the hover panel headed by the verified app name.
//! * [`input`]: clicks and the wheel, sent on the item's channel.
//!
//! Serial: `SHELL:TRAY:SERVICE:PASS`, `SHELL:TRAY:SET app=<id> n=<items>`,
//! `SHELL:TRAY:UPDATE app=<id>`, `SHELL:TRAY:CLEAR app=<id> why=<...>`,
//! `SHELL:TRAY:DENY uid=<n> label=<id> why=<...>`, `SHELL:TRAY:ICON app=<id>
//! source=<...>`, `SHELL:TRAY:EVENT app=<id> kind=<...>`,
//! `SHELL:TRAY:RESTORED n=<items>`, and `UI:RECT name=tray:<app>` under the
//! UI probe.

mod generation;
pub mod icon;
pub mod input;
mod liveness;
pub mod menu;
mod menu_paint;
pub mod paint;
mod service;
pub mod tooltip;

use std::cell::{Cell, RefCell};

use lazyshell::tray::layout::{self, Layout};
use lazyshell::tray::Tray;
use lazyshell::Rect;

use super::ctx::Ctx;
use super::services::App;
use crate::probe;

/// One app's registry facts the tray shows: its verified name and where its
/// package icons are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub id: String,
    pub name: String,
    /// The install directory's `icons/` (`None` for a built-in).
    pub icons_dir: Option<String>,
}

impl Known {
    fn from_app(app: &App) -> Known {
        let icons_dir = app
            .icon
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_owned())
            .filter(|dir| !dir.is_empty());
        Known {
            id: app.id.clone(),
            name: app.name.clone(),
            icons_dir,
        }
    }
}

/// The tray's state, shared through [`Ctx`].
pub struct TrayState {
    pub model: RefCell<Tray>,
    /// Where the cells are, panel-local design pixels.
    pub layout: RefCell<Layout>,
    /// Each app's event channel (this task's end) and the kernel-stamped
    /// label its `Set` came with: later requests for the item must carry the
    /// same label (see the service).
    channels: RefCell<Vec<(String, u64, u32)>>,
    /// The registry rows of the apps with an item.
    known: RefCell<Vec<Known>>,
    service: RefCell<service::TrayService>,
    generation: RefCell<generation::Generation>,
    beat: Cell<u64>,
    /// The cell under the pointer and since when (PIT ticks).
    /// The app whose cell the pointer rests on, and since when (PIT ticks):
    /// keyed by app, so a relayout cannot move it to another app's cell.
    pub hover: RefCell<Option<(String, u64)>>,
    pub tooltip: RefCell<Option<tooltip::Open>>,
    /// The open menu panels: the top level, then at most one submenu.
    pub menus: RefCell<Vec<menu::Panel>>,
    pub pictures: RefCell<icon::Pictures>,
    /// The cells last printed for the UI probe.
    probed: RefCell<Vec<(String, Rect)>>,
}

impl TrayState {
    pub fn new(session: Option<u64>) -> TrayState {
        TrayState {
            model: RefCell::new(Tray::new()),
            layout: RefCell::new(Layout::default()),
            channels: RefCell::new(Vec::new()),
            known: RefCell::new(Vec::new()),
            service: RefCell::new(service::TrayService::default()),
            generation: RefCell::new(generation::Generation::new(session)),
            beat: Cell::new(0),
            hover: RefCell::new(None),
            tooltip: RefCell::new(None),
            menus: RefCell::new(Vec::new()),
            pictures: RefCell::new(icon::Pictures::default()),
            probed: RefCell::new(Vec::new()),
        }
    }

    /// Lay the cells out left of `clock_x`, the left edge of what sits right
    /// of the tray (the Log out button, then the clock); the width the window
    /// entries must leave free on top of those.
    pub fn relayout(&self, clock_x: i32) -> i32 {
        let next = layout::layout(&self.model.borrow(), clock_x);
        let reserved = next.reserved;
        *self.layout.borrow_mut() = next;
        reserved
    }

    /// The verified registry name of `app` (its id while unknown).
    pub fn name(&self, app: &str) -> String {
        self.known
            .borrow()
            .iter()
            .find(|known| known.id == app)
            .map_or_else(|| app.to_owned(), |known| known.name.clone())
    }

    /// Whether `app` is in the registry as last read.
    pub fn knows(&self, app: &str) -> bool {
        self.known.borrow().iter().any(|known| known.id == app)
    }

    /// The `icons/` directory of `app`'s package.
    pub fn icons_dir(&self, app: &str) -> Option<String> {
        self.known
            .borrow()
            .iter()
            .find(|known| known.id == app)
            .and_then(|known| known.icons_dir.clone())
    }

    /// The event channel of `app`'s item.
    pub fn channel(&self, app: &str) -> Option<u64> {
        self.channels
            .borrow()
            .iter()
            .find(|(owner, _, _)| owner == app)
            .map(|(_, handle, _)| *handle)
    }

    /// The label `app`'s item was set with.
    pub fn pinned_label(&self, app: &str) -> Option<u32> {
        self.channels
            .borrow()
            .iter()
            .find(|(owner, _, _)| owner == app)
            .map(|(_, _, label)| *label)
    }

    /// Keep `handle` as `app`'s channel, closing the one it replaces.
    fn keep_channel(&self, app: &str, handle: u64, label: u32) {
        let old = self.take_channel(app);
        if let Some(old) = old {
            let _ = crate::sys::msg_close(old);
        }
        self.channels
            .borrow_mut()
            .push((app.to_owned(), handle, label));
    }

    /// Forget (and close) `app`'s channel.
    fn drop_channel(&self, app: &str) {
        if let Some(handle) = self.take_channel(app) {
            let _ = crate::sys::msg_close(handle);
        }
    }

    fn take_channel(&self, app: &str) -> Option<u64> {
        let mut channels = self.channels.borrow_mut();
        let index = channels.iter().position(|(owner, _, _)| owner == app)?;
        Some(channels.remove(index).1)
    }

    /// Re-read the registry rows of the apps in the tray (names, icons).
    fn refresh_known(&self, ctx: &Ctx) {
        let Ok(apps) = super::services::list_apps() else {
            ctx.note("tray-apps", || String::from("SHELL:TRAY:APPS:FAIL"));
            return;
        };
        *self.known.borrow_mut() = apps.iter().map(Known::from_app).collect();
    }
}

/// One heartbeat of the tray: serve requests, ping channels, follow the
/// generation, open or close the tooltip, pulse attention items.
pub fn pump<M: 'static>(ctx: &std::rc::Rc<Ctx>, ui: &xui_core::app::Ui<M>) {
    let tray = &ctx.tray;
    let changed = tray.service.borrow_mut().pump(ctx);
    let dropped = liveness::pump(ctx);
    if changed || dropped {
        tray.pictures
            .borrow_mut()
            .forget_unused(&tray.model.borrow());
        ctx.bar_changed();
    }
    tooltip::pump(ctx, ui);
    // The attention pulse: repaint twice a second while an item asks for it.
    let beat = crate::sys::clock_ticks() / paint::PULSE_TICKS;
    if tray.beat.replace(beat) != beat && paint::any_attention(&tray.model.borrow()) {
        ctx.repaint_bar();
    }
}

/// Print the cells for the UI probe when they moved.
pub fn probe(ctx: &Ctx) {
    if !probe::enabled() {
        return;
    }
    let cells: Vec<(String, Rect)> = ctx
        .tray
        .layout
        .borrow()
        .cells
        .iter()
        .map(|cell| (cell.app.clone(), cell.rect.offset(0, ctx.bar_y())))
        .collect();
    if *ctx.tray.probed.borrow() == cells {
        return;
    }
    for (app, rect) in &cells {
        let rect = ctx.to_screen(*rect);
        probe::rect(&format!("tray:{app}"), rect.x, rect.y, rect.w, rect.h);
    }
    *ctx.tray.probed.borrow_mut() = cells;
}
