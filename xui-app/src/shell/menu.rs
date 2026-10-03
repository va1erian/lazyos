//! The start menu: a panel created on demand above the "LazyOS" button and
//! destroyed when it closes.
//!
//! Opening re-reads the rows (`sys/ui/menu` and `init`'s installed apps), so a
//! change made in Settings or a package installed a moment ago shows at once.
//! The panel is created rather than kept hidden off-screen because the
//! compositor clamps a panel on screen (`PlaceSurface`), and creating one is a
//! single surface plus one buffer. Ctrl+Esc/Super (`StartMenu`) and the button
//! toggle it; `Dismiss` (a press outside every panel), choosing a row or the
//! button again close it. The power rows at the bottom first turn into a
//! confirmation and keep the menu open ([`super::power`]). The installed
//! apps' category section scrolls with the wheel when it does not fit
//! (`lazyshell::menu::Menu::scroll_by`), with a thin bar on its right edge.

use std::rc::Rc;

use lazyshell::menu::{Action, Choice, BANNER_W, PAD, ROW_H, WIDTH};
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
/// Section rows one wheel notch scrolls, and the notch size.
const WHEEL_ROWS: i64 = 3;
const WHEEL_NOTCH: i64 = 120;
/// Width of the section's scroll bar.
const BAR_W: i32 = 3;

/// A start-menu message.
pub enum MenuMsg {
    Repaint,
    Move(i32, i32),
    Leave,
    /// A left press; `true` for the second press of a double click.
    Press(i32, i32, bool),
    /// A vertical wheel turn (positive: away from the user, scrolls up).
    Wheel(i16),
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
    let (x, y) = (x * ctx.scale(), y * ctx.scale());
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
            } => Some(MenuMsg::Press(x, y, false)),
            Event::MouseDoubleClick {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => Some(MenuMsg::Press(x, y, true)),
            Event::MouseWheel {
                delta,
                horizontal: false,
                ..
            } => Some(MenuMsg::Wheel(delta)),
            _ => None,
        });
        MenuApp { ctx, root }
    }

    /// Act on the enabled row under `(x, y)`: launch an app (closing the
    /// menu), or step the power rows. A press on the banner or a disabled
    /// row does nothing. Returns whether the menu must repaint.
    fn press(&self, x: i32, y: i32, repeat: bool) -> bool {
        let (choice, origin) = {
            let mut menu = self.ctx.menu.borrow_mut();
            let Some(index) = menu.row_at(x, y) else {
                return false;
            };
            let (ox, oy) = menu.origin(self.ctx.screen.1);
            let origin = menu.row_rect(index).map(|rect| rect.offset(ox, oy));
            (menu.choose(index, repeat), origin)
        };
        match choice {
            Choice::Nothing => false,
            Choice::Launch(app) => {
                close(&self.ctx);
                let _ = self.ctx.launch(&app, origin);
                false
            }
            Choice::Confirming(_) => {
                super::power::confirming();
                true
            }
            Choice::Request(power) => {
                close(&self.ctx);
                super::power::request(power);
                false
            }
            Choice::Close => {
                close(&self.ctx);
                false
            }
        }
    }
}

impl App for MenuApp {
    type Msg = MenuMsg;

    fn update(&mut self, msg: MenuMsg, ui: &mut Ui<MenuMsg>) {
        // Pointer events arrive in screen pixels; the menu model is in
        // design pixels.
        let msg = match msg {
            MenuMsg::Move(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                MenuMsg::Move(x, y)
            }
            MenuMsg::Press(x, y, repeat) => {
                let (x, y) = self.ctx.to_design(x, y);
                MenuMsg::Press(x, y, repeat)
            }
            other => other,
        };
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
            MenuMsg::Press(x, y, repeat) => {
                if self.press(x, y, repeat) {
                    ui.invalidate(self.root.id());
                }
            }
            MenuMsg::Wheel(delta) => {
                if self.ctx.menu.borrow_mut().scroll_by(wheel_rows(delta)) {
                    self.ctx.menu_hover.set(None);
                    ui.invalidate(self.root.id());
                }
            }
        }
    }
}

/// Section rows a wheel turn of `delta` scrolls (negative: up), at least
/// one per non-zero turn.
fn wheel_rows(delta: i16) -> i64 {
    let rows = (i64::from(delta).abs() * WHEEL_ROWS / WHEEL_NOTCH).max(1);
    match delta {
        0 => 0,
        d if d > 0 => -rows,
        _ => rows,
    }
}

/// A design-pixel shell rectangle as an xui one at scale `s`.
fn rect(r: ShellRect, s: i32) -> Rect {
    Rect::new(r.x * s, r.y * s, (r.x + r.w) * s, (r.y + r.h) * s)
}

/// Paint the banner and the rows. The model is in design pixels; `s` turns
/// every size into screen pixels (docs/hidpi-plan.md).
fn paint(canvas: &mut dyn Canvas, ctx: &Ctx) {
    let palette = ctx.theme.borrow().palette();
    let s = ctx.scale();
    let bounds = canvas.bounds();
    canvas.clear(color(palette.overlay_bg));
    canvas.stroke_rect(bounds, color(palette.overlay_border), s as f32);

    // The banner: "LazyOS" read top to bottom, one letter per line, at its
    // foot (the canvas has no rotation).
    let banner = Rect::new(
        bounds.left,
        bounds.top,
        bounds.left + BANNER_W * s,
        bounds.bottom,
    );
    canvas.fill_rect(banner, color(palette.overlay_selected));
    let letter = TextStyle::new(color(0xFF_FF_FF), BANNER_TEXT)
        .bold()
        .centered()
        .middle();
    let word = "LazyOS";
    let step = 16 * s;
    let top = banner.bottom - 8 * s - step * word.len() as i32;
    for (i, ch) in word.chars().enumerate() {
        let y = top + step * i as i32;
        let cell = Rect::new(banner.left, y, banner.right, y + step);
        canvas.draw_text(&ch.to_string(), cell, &letter);
    }

    let menu = ctx.menu.borrow();
    let hover = ctx.menu_hover.get();
    for (index, row) in menu.rows().iter().enumerate() {
        let Some(area) = menu.row_rect(index).map(|r| rect(r, s)) else {
            continue;
        };
        let label = Rect::new(
            area.left + LABEL_PAD * s,
            area.top,
            area.right - 4 * s,
            area.bottom,
        );
        if row.action == Action::Header {
            // A category title: bold, in the banner's colour, never lit.
            canvas.push_clip(label);
            let ink = color(palette.overlay_selected);
            canvas.draw_text(
                &row.label,
                label,
                &TextStyle::new(ink, TEXT).bold().middle(),
            );
            canvas.pop_clip();
            continue;
        }
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
        canvas.push_clip(label);
        canvas.draw_text(
            &row.label,
            label,
            &TextStyle::new(color(ink), TEXT).middle(),
        );
        canvas.pop_clip();
    }
    if let Some(scroll) = menu.scroll() {
        paint_scroll_bar(canvas, scroll, s, color(palette.overlay_selected));
    }
}

/// The installed section's scroll bar: a thumb on the panel's right edge,
/// spanning the section's visible rows, as tall as their share of it.
fn paint_scroll_bar(
    canvas: &mut dyn Canvas,
    scroll: lazyshell::menu::Scroll,
    s: i32,
    ink: xui_core::Color,
) {
    let track = scroll.shown as i32 * ROW_H;
    let total = scroll.total.max(1) as i32;
    let top = PAD + track * scroll.first as i32 / total;
    let height = (track * scroll.shown as i32 / total).max(ROW_H / 2);
    let right = WIDTH - 2;
    let bar = ShellRect::new(right - BAR_W, top, BAR_W, height);
    canvas.fill_rect(rect(bar, s), ink);
}
