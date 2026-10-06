//! A tray item's menu (docs/tray-plan.md section 7.2): the app's declarative
//! rows rendered by the shell as a panel with the start menu's look, one
//! submenu level in a second panel beside it, and the shell's own
//! **Quit <App>** row at the bottom (`lazyshell::tray::menu`).
//!
//! Picking an app row sends `MenuItem(id, checked)` on the item's channel;
//! the app answers with `Update` when a check, a radio or a label changed.
//! Quit asks `init.Stop` for the app, off the UI thread (a `Stop` waits for
//! the app to exit), and drops the item. A press outside every panel
//! (`Dismiss`), a pick or opening another menu closes it.
//!
//! Panels never take keyboard focus in `xuid`, so the menu is driven by the
//! pointer, as the start menu is.
//!
//! Serial: `SHELL:TRAY:MENU:OPEN app=<id> rows=<n>`, `SHELL:TRAY:MENU:PICK
//! app=<id> id=<n> checked=<bool>`, `SHELL:TRAY:MENU:CLOSE`,
//! `SHELL:TRAY:QUIT app=<id> stopped=<n>` (or `:FAIL err=<n>`), and
//! `UI:RECT name=traymenu:<label>` per row under the UI probe.

use std::cell::Cell;
use std::rc::Rc;

use lazyshell::tray::menu::{default_pick, Pick, TrayMenu, WIDTH};
use lazyshell::Rect as ShellRect;
use messenger_generated::os_lazy_shell_tray_events_v1 as events;
use xui_core::app::{App, Ui, WindowHandle};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::{Control, Dip, MouseButton};

use super::super::ctx::Ctx;
use super::super::services;
use super::liveness;
use crate::client_window::SurfaceRole;
use crate::probe;

/// One open panel: the top level (depth 0) or a submenu (depth 1).
pub struct Panel {
    pub app: String,
    pub rows: TrayMenu,
    /// Top-left corner, screen design pixels.
    pub origin: (i32, i32),
    pub hover: Cell<Option<usize>>,
    handle: Option<WindowHandle<PanelMsg>>,
}

/// A panel message; the depth says which panel.
pub enum PanelMsg {
    Move(i32, i32),
    Leave,
    Press(i32, i32),
}

/// Open `app`'s menu above its cell, replacing any open tray menu.
pub fn open<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, app: &str) {
    close(ctx);
    let Some(cell) = ctx.tray.layout.borrow().cell(app) else {
        return;
    };
    let rows = {
        let model = ctx.tray.model.borrow();
        let custom = model.get(app).and_then(|entry| entry.custom.as_ref());
        let menu = custom.map_or(&[][..], |item| item.menu.as_slice());
        TrayMenu::top(menu, &ctx.tray.name(app))
    };
    let cell = cell.offset(0, ctx.bar_y());
    let origin = TrayMenu::origin(cell, rows.height(), ctx.screen, ctx.bar_y());
    let count = rows.rows().len();
    if show(ctx, ui, app, rows, origin) {
        println!("SHELL:TRAY:MENU:OPEN app={app} rows={count}");
    }
}

/// Close every tray menu panel.
pub fn close(ctx: &Ctx) {
    let panels: Vec<Panel> = ctx.tray.menus.borrow_mut().drain(..).collect();
    if panels.is_empty() {
        return;
    }
    for panel in panels {
        if let Some(handle) = panel.handle {
            handle.close();
        }
    }
    println!("SHELL:TRAY:MENU:CLOSE");
}

/// What a `DefaultItem` click does for `app`: run its default row.
pub fn run_default(ctx: &Ctx, app: &str) {
    let pick = {
        let model = ctx.tray.model.borrow();
        model
            .get(app)
            .and_then(|entry| entry.custom.as_ref())
            .and_then(|item| default_pick(&item.menu))
    };
    if let Some(Pick::Item { id, checked }) = pick {
        send_pick(ctx, app, id, checked);
    }
}

/// Create one panel at `origin` and keep it; `false` when the compositor
/// refused it.
fn show<M: 'static>(
    ctx: &Rc<Ctx>,
    ui: &Ui<M>,
    app: &str,
    rows: TrayMenu,
    origin: (i32, i32),
) -> bool {
    let depth = ctx.tray.menus.borrow().len();
    let s = ctx.scale();
    ctx.backend.set_next_role(SurfaceRole::Panel {
        x: origin.0 * s,
        y: origin.1 * s,
    });
    let height = rows.height();
    print_rows(ctx, &rows, origin);
    ctx.tray.menus.borrow_mut().push(Panel {
        app: app.to_owned(),
        rows,
        origin,
        hover: Cell::new(None),
        handle: None,
    });
    let spec = PlatformSpec::new("LazyOS").size(Dip(WIDTH as f32), Dip(height as f32));
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| PanelApp::build(built, depth, ui)) {
        Ok(handle) => {
            if let Some(panel) = ctx.tray.menus.borrow_mut().get_mut(depth) {
                panel.handle = Some(handle);
            }
            true
        }
        Err(error) => {
            ctx.tray.menus.borrow_mut().truncate(depth);
            ctx.note("tray-menu", || format!("SHELL:TRAY:MENU:FAIL {error}"));
            false
        }
    }
}

/// Probe lines for a panel's rows (screen pixels).
fn print_rows(ctx: &Ctx, rows: &TrayMenu, origin: (i32, i32)) {
    if !probe::enabled() {
        return;
    }
    for (index, row) in rows.rows().iter().enumerate() {
        if let (Some(rect), false) = (rows.row_rect(index), row.label.is_empty()) {
            let rect = ctx.to_screen(rect.offset(origin.0, origin.1));
            probe::rect(
                &format!("traymenu:{}", row.label),
                rect.x,
                rect.y,
                rect.w,
                rect.h,
            );
        }
    }
}

/// Send `MenuItem(id, checked)` to `app`.
fn send_pick(ctx: &Ctx, app: &str, id: u32, checked: bool) {
    let Some(channel) = ctx.tray.channel(app) else {
        return;
    };
    let Ok(body) = events::encode_menu_item_args(&events::MenuItemArgs { id, checked }) else {
        return;
    };
    println!("SHELL:TRAY:MENU:PICK app={app} id={id} checked={checked}");
    if liveness::send(channel, events::METHOD_MENUITEM, body).is_err() {
        liveness::gone(ctx, app);
        ctx.bar_changed();
    }
}

/// The shell's Quit row: stop the app through `init`, on a thread of its
/// own (a `Stop` replies once the app has exited), and drop its item now.
fn quit(ctx: &Ctx, app: &str) {
    let id = app.to_owned();
    std::thread::spawn(move || match services::stop(&id) {
        Ok(stopped) => println!("SHELL:TRAY:QUIT app={id} stopped={stopped}"),
        Err(code) => println!("SHELL:TRAY:QUIT:FAIL app={id} err={}", -code),
    });
    ctx.tray.model.borrow_mut().clear(app);
    ctx.tray.drop_channel(app);
    println!("SHELL:TRAY:CLEAR app={app} why=quit");
    ctx.bar_changed();
}

/// A tray menu panel's app.
struct PanelApp {
    ctx: Rc<Ctx>,
    depth: usize,
    root: Control<PanelMsg>,
}

impl PanelApp {
    fn build(ctx: Rc<Ctx>, depth: usize, ui: &mut Ui<PanelMsg>) -> PanelApp {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("tray menu node");
        {
            let ctx = Rc::clone(&ctx);
            root.set_painter(Rc::new(move |canvas| {
                super::menu_paint::paint(canvas, &ctx, depth)
            }));
        }
        root.on_events(|event| match *event {
            Event::MouseMove { x, y, .. } => Some(PanelMsg::Move(x, y)),
            Event::MouseLeave => Some(PanelMsg::Leave),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left | MouseButton::Right,
                ..
            } => Some(PanelMsg::Press(x, y)),
            _ => None,
        });
        PanelApp { ctx, depth, root }
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.ctx
            .tray
            .menus
            .borrow()
            .get(self.depth)?
            .rows
            .row_at(x, y)
    }

    fn press(&self, ui: &Ui<PanelMsg>, x: i32, y: i32) {
        let chosen = {
            let menus = self.ctx.tray.menus.borrow();
            let Some(panel) = menus.get(self.depth) else {
                return;
            };
            let Some(index) = panel.rows.row_at(x, y) else {
                return;
            };
            let rect = panel.rows.row_rect(index).unwrap_or_default();
            panel
                .rows
                .pick(index)
                .map(|pick| (pick, panel.app.clone(), panel.origin, rect))
        };
        let Some((pick, app, origin, rect)) = chosen else {
            return;
        };
        match pick {
            Pick::Item { id, checked } => {
                close(&self.ctx);
                send_pick(&self.ctx, &app, id, checked);
            }
            Pick::Quit => {
                close(&self.ctx);
                quit(&self.ctx, &app);
            }
            Pick::Submenu { id } => self.open_submenu(ui, &app, id, origin, rect),
        }
    }

    /// Open the submenu of row `id` beside its row (left of the menu when
    /// there is no room on the right), replacing an open one.
    fn open_submenu(
        &self,
        ui: &Ui<PanelMsg>,
        app: &str,
        id: u32,
        origin: (i32, i32),
        row: ShellRect,
    ) {
        let rows = {
            let model = self.ctx.tray.model.borrow();
            let custom = model.get(app).and_then(|entry| entry.custom.as_ref());
            TrayMenu::submenu(custom.map_or(&[][..], |item| item.menu.as_slice()), id)
        };
        let panels: Vec<Panel> = self
            .ctx
            .tray
            .menus
            .borrow_mut()
            .drain(self.depth + 1..)
            .collect();
        for panel in panels {
            if let Some(handle) = panel.handle {
                handle.close();
            }
        }
        let right = origin.0 + WIDTH;
        let x = if right + WIDTH <= self.ctx.screen.0 {
            right
        } else {
            (origin.0 - WIDTH).max(0)
        };
        let y = (origin.1 + row.y)
            .min(self.ctx.bar_y() - rows.height())
            .max(0);
        show(&self.ctx, ui, app, rows, (x, y));
    }
}

impl App for PanelApp {
    type Msg = PanelMsg;

    fn update(&mut self, msg: PanelMsg, ui: &mut Ui<PanelMsg>) {
        let hover = match msg {
            PanelMsg::Move(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.row_at(x, y)
            }
            PanelMsg::Leave => None,
            PanelMsg::Press(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.press(ui, x, y);
                return;
            }
        };
        let changed = self
            .ctx
            .tray
            .menus
            .borrow()
            .get(self.depth)
            .is_some_and(|panel| panel.hover.replace(hover) != hover);
        if changed {
            ui.invalidate(self.root.id());
        }
    }
}
