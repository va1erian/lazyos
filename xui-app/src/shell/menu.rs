//! The start menu: a panel created on demand above the "LazyOS" button and
//! destroyed when it closes.
//!
//! Opening re-reads the rows (`sys/ui/menu` and `init`'s installed apps), so a
//! change made in Settings or a package installed a moment ago shows at once.
//! The panel is created rather than kept hidden off-screen because the
//! compositor clamps a panel on screen (`PlaceSurface`), and creating one is a
//! single surface plus one buffer. Ctrl+Esc/Super (`StartMenu`) and the button
//! toggle it; `Dismiss` (a press outside every panel), choosing a row or the
//! button again close it.

use std::rc::Rc;

use lazyshell::menu::{BANNER_W, WIDTH};
use lazyshell::Rect as ShellRect;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec, TextStyle};
use xui_core::{Canvas, Control, Dip, MouseButton, Rect};

use super::ctx::Ctx;
use super::theme::color;
use crate::client_window::SurfaceRole;

/// Row text size.
const TEXT: Dip = Dip(12.0);
/// Banner letter size.
const BANNER_TEXT: Dip = Dip(13.0);
/// Gap between the banner and a row's label.
const LABEL_PAD: i32 = 10;

/// A start-menu message.
pub enum MenuMsg {
    Repaint,
    Move(i32, i32),
    Leave,
    Press(i32, i32),
}

/// Open the menu if it is closed, close it if it is open.
pub fn toggle<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    if ctx.menu_window.borrow().is_some() {
        close(ctx);
    } else {
        open(ctx, ui);
    }
}

/// Open the menu (a no-op when it is open): reload its rows, then create the
/// panel with its bottom edge on the taskbar's top edge.
pub fn open<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    if ctx.menu_window.borrow().is_some() {
        return;
    }
    ctx.reload_menu();
    let (height, (x, y)) = {
        let menu = ctx.menu.borrow();
        (menu.height(), menu.origin(ctx.screen.1))
    };
    ctx.backend.set_next_role(SurfaceRole::Panel { x, y });
    let spec = PlatformSpec::new("LazyOS").size(Dip(WIDTH as f32), Dip(height as f32));
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| MenuApp::build(built, ui)) {
        Ok(handle) => {
            *ctx.menu_window.borrow_mut() = Some(handle);
            println!("SHELL:MENU:OPEN");
            ctx.repaint_bar();
        }
        Err(error) => ctx.note("menu-open", || format!("SHELL:MENU:FAIL {error}")),
    }
}

/// Close the menu, if open.
pub fn close(ctx: &Ctx) {
    let Some(handle) = ctx.menu_window.borrow_mut().take() else {
        return;
    };
    handle.close();
    ctx.menu_hover.set(None);
    println!("SHELL:MENU:CLOSE");
    ctx.repaint_bar();
}

/// The start menu window's app.
pub struct MenuApp {
    ctx: Rc<Ctx>,
    root: Control<MenuMsg>,
}

impl MenuApp {
    fn build(ctx: Rc<Ctx>, ui: &mut Ui<MenuMsg>) -> MenuApp {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("menu node");
        {
            let ctx = Rc::clone(&ctx);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &ctx)));
        }
        root.on_events(|event| match *event {
            Event::MouseMove { x, y, .. } => Some(MenuMsg::Move(x, y)),
            Event::MouseLeave => Some(MenuMsg::Leave),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            }
            | Event::MouseDoubleClick {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => Some(MenuMsg::Press(x, y)),
            _ => None,
        });
        MenuApp { ctx, root }
    }

    /// Launch the enabled row under `(x, y)` and close the menu; a press on
    /// the banner or a disabled row does nothing.
    fn press(&self, x: i32, y: i32) {
        let target = {
            let menu = self.ctx.menu.borrow();
            menu.row_at(x, y).and_then(|index| {
                let row = menu.rows().get(index).filter(|row| row.enabled)?;
                let (ox, oy) = menu.origin(self.ctx.screen.1);
                Some((row.app.clone(), menu.row_rect(index)?.offset(ox, oy)))
            })
        };
        if let Some((app, origin)) = target {
            close(&self.ctx);
            let _ = self.ctx.launch(&app, Some(origin));
        }
    }
}

impl App for MenuApp {
    type Msg = MenuMsg;

    fn update(&mut self, msg: MenuMsg, ui: &mut Ui<MenuMsg>) {
        match msg {
            MenuMsg::Repaint => ui.invalidate(self.root.id()),
            MenuMsg::Move(x, y) => {
                let row = self.ctx.menu.borrow().row_at(x, y);
                if self.ctx.menu_hover.replace(row) != row {
                    ui.invalidate(self.root.id());
                }
            }
            MenuMsg::Leave => {
                if self.ctx.menu_hover.replace(None).is_some() {
                    ui.invalidate(self.root.id());
                }
            }
            MenuMsg::Press(x, y) => self.press(x, y),
        }
    }
}

fn rect(r: ShellRect) -> Rect {
    Rect::new(r.x, r.y, r.x + r.w, r.y + r.h)
}

/// Paint the banner and the rows.
fn paint(canvas: &mut dyn Canvas, ctx: &Ctx) {
    let palette = ctx.theme.borrow().palette();
    let bounds = canvas.bounds();
    canvas.clear(color(palette.overlay_bg));
    canvas.stroke_rect(bounds, color(palette.overlay_border), 1.0);

    // The banner: "LazyOS" read top to bottom, one letter per line, at its
    // foot (the canvas has no rotation).
    let banner = Rect::new(
        bounds.left,
        bounds.top,
        bounds.left + BANNER_W,
        bounds.bottom,
    );
    canvas.fill_rect(banner, color(palette.overlay_selected));
    let letter = TextStyle::new(color(0xFF_FF_FF), BANNER_TEXT)
        .bold()
        .centered()
        .middle();
    let word = "LazyOS";
    let step = 16;
    let top = banner.bottom - 8 - step * word.len() as i32;
    for (i, ch) in word.chars().enumerate() {
        let y = top + step * i as i32;
        let cell = Rect::new(banner.left, y, banner.right, y + step);
        canvas.draw_text(&ch.to_string(), cell, &letter);
    }

    let menu = ctx.menu.borrow();
    let hover = ctx.menu_hover.get();
    for (index, row) in menu.rows().iter().enumerate() {
        let Some(area) = menu.row_rect(index).map(rect) else {
            continue;
        };
        let lit = row.enabled && hover == Some(index);
        if lit {
            canvas.fill_rect(area, color(palette.overlay_selected));
        }
        let ink = if !row.enabled {
            uitheme::mix(palette.overlay_text, palette.overlay_bg, 3, 5)
        } else if lit {
            0xFF_FF_FF
        } else {
            palette.overlay_text
        };
        let label = Rect::new(area.left + LABEL_PAD, area.top, area.right - 4, area.bottom);
        canvas.push_clip(label);
        canvas.draw_text(
            &row.label,
            label,
            &TextStyle::new(color(ink), TEXT).middle(),
        );
        canvas.pop_clip();
    }
}
