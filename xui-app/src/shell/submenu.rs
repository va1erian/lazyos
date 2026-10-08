//! A category's submenu: a second panel that flies out to the right of the
//! start menu, listing the category's apps (`lazyshell::menu::Submenu`).
//!
//! Resting the pointer on a category row (or clicking it) opens that
//! category's submenu; moving onto another category switches it, and onto
//! any other row closes it. Choosing an app launches it and closes the whole
//! menu, and so does anything that closes the start menu ([`super::menu::close`]),
//! including a press outside every panel: the submenu is a panel too, so a
//! press inside it is not one. Like the start menu it is created on demand and
//! destroyed when it closes.
//!
//! From the keyboard (issue #648, `super::keys`), Right or Enter on a
//! category row opens its submenu with the first app lit and moves the keys
//! into it; Up/Down move, Enter launches, Left or Escape go back.
//!
//! Serial markers: `SHELL:SUBMENU:OPEN category=<id> apps=<n>`,
//! `SHELL:SUBMENU:CLOSE`, `SHELL:SUBMENU:KEY:SELECT row=<label>` and
//! `SHELL:SUBMENU:KEY:BACK`.

use std::rc::Rc;

use lazyshell::keynav::{self, NavKey, RowKind, Step};
use lazyshell::menu::{Submenu, SUB_WIDTH};
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::{Canvas, Control, Dip, MouseButton};

use super::ctx::Ctx;
use super::menu::{self, paint_row, rect};
use super::theme::{chrome_look, color, fill_bar};
use crate::client_window::SurfaceRole;

/// A submenu message.
pub enum SubMsg {
    Move(i32, i32),
    Leave,
    Press(i32, i32),
    /// A navigation key from the panel-key grab.
    Key(NavKey),
}

/// Open the submenu of the start-menu row `row` (a category row), replacing
/// one open for another category. A no-op when it is already open.
pub fn open<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, row: usize) {
    let Some(next) = ctx.menu.borrow().submenu(row, ctx.screen.1) else {
        return;
    };
    let same = ctx
        .submenu
        .borrow()
        .as_ref()
        .is_some_and(|open| open.category == next.category);
    if same && ctx.submenu_window.borrow().is_some() {
        return;
    }
    close(ctx);
    let (x, y) = next.origin();
    let (id, apps, height) = (next.id, next.rows().len(), next.height());
    *ctx.submenu.borrow_mut() = Some(next);
    ctx.backend.set_next_role(SurfaceRole::Panel {
        x: x * ctx.scale(),
        y: y * ctx.scale(),
    });
    let spec = PlatformSpec::new("LazyOS").size(Dip(SUB_WIDTH as f32), Dip(height as f32));
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| SubmenuApp::build(built, ui)) {
        Ok(handle) => {
            *ctx.submenu_window.borrow_mut() = Some(handle);
            super::probe::submenu(ctx);
            println!("SHELL:SUBMENU:OPEN category={id} apps={apps}");
        }
        Err(error) => {
            ctx.submenu.borrow_mut().take();
            ctx.note("submenu-open", || format!("SHELL:SUBMENU:FAIL {error}"));
        }
    }
    ctx.repaint_menu();
}

/// Close the submenu, if open.
pub fn close(ctx: &Ctx) {
    let handle = ctx.submenu_window.borrow_mut().take();
    ctx.submenu.borrow_mut().take();
    ctx.submenu_hover.set(None);
    ctx.keys_in_submenu.set(false);
    if let Some(handle) = handle {
        handle.close();
        println!("SHELL:SUBMENU:CLOSE");
        ctx.repaint_menu();
    }
}

/// The keyboard opened the submenu: light its first app and send the keys
/// there.
pub fn enter_by_key(ctx: &Ctx) {
    let first = {
        let submenu = ctx.submenu.borrow();
        let Some(sub) = submenu.as_ref() else {
            return;
        };
        keynav::next(&row_kinds(sub), None, true)
    };
    ctx.submenu_hover.set(first);
    ctx.keys_in_submenu.set(true);
    if let Some(handle) = &*ctx.submenu_window.borrow() {
        handle.send(SubMsg::Key(NavKey::Home));
    }
}

/// How each submenu row navigates (a disabled app is skipped).
fn row_kinds(sub: &Submenu) -> Vec<RowKind> {
    sub.rows()
        .iter()
        .map(|row| {
            if row.enabled {
                RowKind::Item
            } else {
                RowKind::Inert
            }
        })
        .collect()
}

/// The submenu window's app.
pub struct SubmenuApp {
    ctx: Rc<Ctx>,
    root: Control<SubMsg>,
}

impl SubmenuApp {
    fn build(ctx: Rc<Ctx>, ui: &mut Ui<SubMsg>) -> SubmenuApp {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("submenu node");
        {
            let ctx = Rc::clone(&ctx);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &ctx)));
        }
        root.on_events(|event| match *event {
            Event::MouseMove { x, y, .. } => Some(SubMsg::Move(x, y)),
            Event::MouseLeave => Some(SubMsg::Leave),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => Some(SubMsg::Press(x, y)),
            _ => None,
        });
        SubmenuApp { ctx, root }
    }

    /// The row under design-pixel `(x, y)`.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.ctx.submenu.borrow().as_ref()?.row_at(x, y)
    }

    /// Launch the app under `(x, y)`, zooming its window from the row, and
    /// close the whole menu.
    fn press(&self, x: i32, y: i32) {
        if let Some(index) = self.row_at(x, y) {
            self.launch(index);
        }
    }

    /// A navigation key: move the lit app, launch it, or go back to the
    /// start menu. Returns whether the panel must repaint.
    fn key(&self, key: NavKey) -> bool {
        let kinds = match self.ctx.submenu.borrow().as_ref() {
            Some(sub) => row_kinds(sub),
            None => return false,
        };
        match keynav::step(&kinds, self.ctx.submenu_hover.get(), key, true) {
            Step::Select(index) => {
                self.ctx.submenu_hover.set(Some(index));
                if let Some(row) = self
                    .ctx
                    .submenu
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.rows().get(index))
                {
                    println!("SHELL:SUBMENU:KEY:SELECT row={}", row.label);
                }
                true
            }
            Step::Activate(index) => {
                self.launch(index);
                false
            }
            Step::Back => {
                println!("SHELL:SUBMENU:KEY:BACK");
                close(&self.ctx);
                false
            }
            Step::OpenChild(_) | Step::Close | Step::Nothing => false,
        }
    }

    /// Launch the app of row `index`, zooming its window from the row, and
    /// close the whole menu.
    fn launch(&self, index: usize) {
        let chosen = {
            let submenu = self.ctx.submenu.borrow();
            let Some(sub) = submenu.as_ref() else {
                return;
            };
            let (ox, oy) = sub.origin();
            let origin = sub.row_rect(index).map(|rect| rect.offset(ox, oy));
            sub.app(index).map(|app| (app.to_owned(), origin))
        };
        if let Some((app, origin)) = chosen {
            menu::close(&self.ctx);
            let _ = self.ctx.launch(&app, origin);
        }
    }
}

impl App for SubmenuApp {
    type Msg = SubMsg;

    fn update(&mut self, msg: SubMsg, ui: &mut Ui<SubMsg>) {
        match msg {
            SubMsg::Move(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                let row = self.row_at(x, y);
                if self.ctx.submenu_hover.replace(row) != row {
                    ui.invalidate(self.root.id());
                }
            }
            SubMsg::Leave => {
                if self.ctx.submenu_hover.replace(None).is_some() {
                    ui.invalidate(self.root.id());
                }
            }
            SubMsg::Press(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.press(x, y);
            }
            SubMsg::Key(key) => {
                if self.key(key) {
                    ui.invalidate(self.root.id());
                }
            }
        }
    }
}

/// Paint the submenu's rows on the menu's panel colours.
fn paint(canvas: &mut dyn Canvas, ctx: &Ctx) {
    let palette = ctx.theme.borrow().palette();
    let s = ctx.scale();
    let deco = chrome_look(ctx.theme.borrow().is_dark());
    let bounds = canvas.bounds();
    fill_bar(canvas, bounds, palette.overlay_bg, &deco);
    canvas.stroke_rect(bounds, color(palette.overlay_border), s as f32);
    let submenu = ctx.submenu.borrow();
    let Some(sub): Option<&Submenu> = submenu.as_ref() else {
        return;
    };
    let hover = ctx.submenu_hover.get();
    for (index, row) in sub.rows().iter().enumerate() {
        if let Some(area) = sub.row_rect(index).map(|r| rect(r, s)) {
            paint_row(canvas, ctx, area, row, hover == Some(index), false);
        }
    }
}
