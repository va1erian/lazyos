//! The shell's UI probe lines (issue #538, [`crate::probe`]): the start button
//! and every start-menu and submenu row, as screen rectangles, so a session
//! script clicks `{"click_at": {"menu": "Accessories"}}` instead of replaying
//! measured relative moves. Printed only in a `LAZYOS_UI_PROBE=1` image.
//!
//! Names: `taskbar:start`, and `menu:<label>` for each visible row of the
//! menu (categories, configured rows, power rows) and of the open submenu
//! (its apps). Coordinates are physical screen pixels.

use lazyshell::menu::Row;
use lazyshell::taskbar::START_BUTTON;
use lazyshell::Rect as ShellRect;

use super::ctx::Ctx;
use crate::probe;

/// Print `rect` (design pixels, offset by `origin`) as `name`.
fn print(ctx: &Ctx, name: &str, origin: (i32, i32), rect: ShellRect) {
    let s = ctx.scale();
    probe::rect(
        name,
        (origin.0 + rect.x) * s,
        (origin.1 + rect.y) * s,
        rect.w * s,
        rect.h * s,
    );
}

/// The start button, once the taskbar is placed.
pub fn start_button(ctx: &Ctx) {
    if probe::enabled() {
        print(ctx, "taskbar:start", (0, ctx.bar_y()), START_BUTTON);
    }
}

/// Every row of a menu panel at `origin`, given each row's local rectangle.
fn rows(ctx: &Ctx, origin: (i32, i32), rows: &[Row], rect: impl Fn(usize) -> Option<ShellRect>) {
    for (index, row) in rows.iter().enumerate() {
        if let Some(local) = rect(index) {
            print(ctx, &format!("menu:{}", row.label), origin, local);
        }
    }
}

/// The start menu's rows, after it opened or scrolled.
pub fn menu(ctx: &Ctx) {
    if !probe::enabled() {
        return;
    }
    let menu = ctx.menu.borrow();
    rows(ctx, menu.origin(ctx.screen.1), menu.rows(), |i| {
        menu.row_rect(i)
    });
}

/// The open submenu's rows.
pub fn submenu(ctx: &Ctx) {
    if !probe::enabled() {
        return;
    }
    if let Some(sub) = ctx.submenu.borrow().as_ref() {
        rows(ctx, sub.origin(), sub.rows(), |i| sub.row_rect(i));
    }
}
