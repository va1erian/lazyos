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
//! Panels never take keyboard focus in `xuid`; while a tray menu is open
//! the shell holds the compositor's panel-key grab instead (issue #648,
//! `super::super::keys`) and the deepest panel gets the keys: Up/Down move,
//! Right or Enter open a submenu, Enter picks (check, radio, Open, Quit),
//! Left or Escape close one level.
//!
//! Serial: `SHELL:TRAY:MENU:OPEN app=<id> rows=<n>`, `SHELL:TRAY:MENU:PICK
//! app=<id> id=<n> checked=<bool>`, `SHELL:TRAY:MENU:CLOSE`,
//! `SHELL:TRAY:QUIT app=<id> stopped=<n>` (or `:FAIL err=<n>`),
//! `SHELL:TRAY:MENU:KEY:SELECT row=<label>`, `SHELL:TRAY:MENU:KEY:BACK`, and
//! `UI:RECT name=traymenu:<label>` per row under the UI probe.

use std::cell::Cell;
use std::rc::Rc;

use lazyshell::keynav::{self, NavKey, RowKind, Step};
use lazyshell::tray::menu::{default_pick, Kind, Pick, TrayMenu, WIDTH};
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
    /// A navigation key from the panel-key grab.
    Key(NavKey),
}

/// Open `app`'s menu above its cell, replacing any open tray menu.
pub fn open<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, app: &str) {
    close(ctx);
    let Some(cell) = ctx.tray.layout.borrow().cell(app) else {
        return;
    };
    let rows = {
        let model = ctx.tray.model.borrow();
        let name = ctx.tray.name(app);
        match model.get(app).and_then(|entry| entry.custom.as_ref()) {
            Some(item) => TrayMenu::top(&item.menu, &name),
            // A resident app's default item: Open, then Quit.
            None => TrayMenu::default_item(&name),
        }
    };
    let cell = cell.offset(0, ctx.bar_y());
    let origin = TrayMenu::origin(cell, rows.height(), ctx.screen, ctx.bar_y());
    let count = rows.rows().len();
    if show(ctx, ui, app, rows, origin) {
        println!("SHELL:TRAY:MENU:OPEN app={app} rows={count}");
    }
    super::super::keys::sync(ctx);
}

/// Send `key` to the deepest open tray menu panel; whether one was open.
pub fn key(ctx: &Ctx, key: NavKey) -> bool {
    let menus = ctx.tray.menus.borrow();
    let Some(panel) = menus.last() else {
        return false;
    };
    if let Some(handle) = &panel.handle {
        handle.send(PanelMsg::Key(key));
    }
    true
}

/// How each tray menu row navigates.
fn row_kinds(rows: &TrayMenu) -> Vec<RowKind> {
    rows.rows()
        .iter()
        .map(|row| match row.kind {
            _ if !row.enabled => RowKind::Inert,
            Kind::Separator => RowKind::Inert,
            Kind::Submenu { .. } => RowKind::Parent,
            _ => RowKind::Item,
        })
        .collect()
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
    super::super::keys::sync(ctx);
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
    match liveness::send(channel, events::METHOD_MENUITEM, body) {
        Ok(()) => {}
        // A full queue is a busy app, not a dead one (as for clicks).
        Err(code) if code == -crate::sys::errno::EAGAIN => {
            println!("SHELL:TRAY:MENU:PICK:FAIL app={app} err={}", -code);
        }
        Err(_) => {
            liveness::gone(ctx, app);
            ctx.bar_changed();
        }
    }
}

/// A default item's Open (its row, or a click): launch the app through
/// `init`, zooming from its cell. A running resident app gets it as
/// `Reopen`.
pub fn open_app(ctx: &Ctx, app: &str) {
    let origin = ctx
        .tray
        .layout
        .borrow()
        .cell(app)
        .map(|cell| cell.offset(0, ctx.bar_y()));
    let _ = ctx.launch(app, origin);
}

/// The shell's Quit row: stop the app through `init`, on a thread of its
/// own (a `Stop` may wait for the app to exit). The item stays until the app
/// is really gone: its channel's peer dies with it (the next `Ping` drops
/// the item), so an app a refused or failed stop leaves running keeps it.
fn quit(app: &str) {
    let id = app.to_owned();
    println!("SHELL:TRAY:QUIT:ASK app={id}");
    std::thread::spawn(move || match services::stop(&id) {
        Ok(stopped) => println!("SHELL:TRAY:QUIT app={id} stopped={stopped}"),
        Err(code) => println!("SHELL:TRAY:QUIT:FAIL app={id} err={}", -code),
    });
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
        if let Some(index) = self.row_at(x, y) {
            self.pick(ui, index);
        }
    }

    /// A navigation key on this panel. Returns whether it must repaint.
    fn key(&self, ui: &Ui<PanelMsg>, key: NavKey) -> bool {
        let (kinds, lit) = {
            let menus = self.ctx.tray.menus.borrow();
            let Some(panel) = menus.get(self.depth) else {
                return false;
            };
            (row_kinds(&panel.rows), panel.hover.get())
        };
        match keynav::step(&kinds, lit, key, self.depth > 0) {
            Step::Select(index) => {
                let menus = self.ctx.tray.menus.borrow();
                if let Some(panel) = menus.get(self.depth) {
                    panel.hover.set(Some(index));
                    let label = &panel.rows.rows()[index].label;
                    println!("SHELL:TRAY:MENU:KEY:SELECT row={label}");
                }
                true
            }
            Step::OpenChild(index) | Step::Activate(index) => {
                self.pick(ui, index);
                // A submenu opened from the keyboard starts on its first row.
                if self.ctx.tray.menus.borrow().len() > self.depth + 1 {
                    key_into_child(&self.ctx);
                }
                false
            }
            Step::Back => {
                println!("SHELL:TRAY:MENU:KEY:BACK");
                close_from(&self.ctx, self.depth);
                false
            }
            Step::Close => {
                close(&self.ctx);
                false
            }
            Step::Nothing => false,
        }
    }

    /// Pick row `index` (a click or Enter).
    fn pick(&self, ui: &Ui<PanelMsg>, index: usize) {
        let chosen = {
            let menus = self.ctx.tray.menus.borrow();
            let Some(panel) = menus.get(self.depth) else {
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
                quit(&app);
            }
            Pick::Open => {
                close(&self.ctx);
                open_app(&self.ctx, &app);
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

/// Light the first row of the deepest panel (just opened by a key).
fn key_into_child(ctx: &Ctx) {
    key(ctx, NavKey::Home);
}

/// Close the panels from `depth` down (a submenu going back to its parent).
fn close_from(ctx: &Ctx, depth: usize) {
    let panels: Vec<Panel> = {
        let mut menus = ctx.tray.menus.borrow_mut();
        let at = depth.min(menus.len());
        menus.drain(at..).collect()
    };
    for panel in panels {
        if let Some(handle) = panel.handle {
            handle.close();
        }
    }
    super::super::keys::sync(ctx);
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
            PanelMsg::Key(key) => {
                if self.key(ui, key) {
                    ui.invalidate(self.root.id());
                }
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
