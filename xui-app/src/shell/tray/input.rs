//! Clicks and the wheel on a tray cell, sent to the app on its item's
//! channel (docs/tray-plan.md section 7.2). Menus (stage T2) and flyout
//! tokens (T4) come later: for now a primary click sends `Activate` with no
//! token and a secondary click sends `SecondaryActivate`.

use lazyshell::tray::layout::Hit;
use messenger_generated::os_lazy_shell_tray_events_v1 as events;

use super::super::ctx::{BarHover, Ctx};
use super::liveness;

/// Which input reached a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Primary,
    Secondary,
    Wheel(i32),
}

/// What panel-local design-pixel `(x, y)` is over on the tray.
pub fn hit(ctx: &Ctx, x: i32, y: i32) -> Option<BarHover> {
    let layout = ctx.tray.layout.borrow();
    match layout.hit(x, y)? {
        Hit::Chevron => Some(BarHover::Chevron),
        Hit::Item(app) => layout
            .cells
            .iter()
            .position(|cell| cell.app == app)
            .map(BarHover::Tray),
    }
}

/// The bar's hover moved: remember since when the pointer rests on a cell
/// (the tooltip opens after a pause).
pub fn hovered(ctx: &Ctx, hover: Option<BarHover>) {
    let next = match hover {
        Some(BarHover::Tray(cell)) => ctx
            .tray
            .layout
            .borrow()
            .cells
            .get(cell)
            .map(|c| (c.app.clone(), crate::sys::clock_ticks())),
        _ => None,
    };
    *ctx.tray.hover.borrow_mut() = next;
}

/// `input` on the cell at layout index `cell`.
pub fn on_cell(ctx: &Ctx, cell: usize, input: Input) {
    let app = ctx
        .tray
        .layout
        .borrow()
        .cells
        .get(cell)
        .map(|c| c.app.clone());
    if let Some(app) = app {
        super::tooltip::close(ctx);
        deliver(ctx, &app, input);
    }
}

/// Deliver `input` on `app`'s cell. An app with no channel (a resident
/// app's default item, stage T3) gets nothing yet; a broken channel drops
/// the item.
pub fn deliver(ctx: &Ctx, app: &str, input: Input) {
    let Some(channel) = ctx.tray.channel(app) else {
        return;
    };
    let anchor = anchor(ctx, app);
    let (kind, method, body) = match input {
        Input::Primary => (
            "activate",
            events::METHOD_ACTIVATE,
            events::encode_activate_args(&events::ActivateArgs { anchor, popup: 0 }),
        ),
        Input::Secondary => (
            "secondary",
            events::METHOD_SECONDARYACTIVATE,
            events::encode_secondary_activate_args(&events::SecondaryActivateArgs { anchor }),
        ),
        Input::Wheel(delta) => (
            "scroll",
            events::METHOD_SCROLL,
            events::encode_scroll_args(&events::ScrollArgs { delta }),
        ),
    };
    let Ok(body) = body else {
        return;
    };
    match liveness::send(channel, method, body) {
        Ok(()) => println!("SHELL:TRAY:EVENT app={app} kind={kind}"),
        Err(code) => {
            println!("SHELL:TRAY:EVENT:FAIL app={app} kind={kind} err={}", -code);
            // A full queue is a busy app; anything else is a dead channel.
            if code != -crate::sys::errno::EAGAIN {
                liveness::gone(ctx, app);
                ctx.bar_changed();
            }
        }
    }
}

/// `app`'s cell in screen pixels (empty when it is in the overflow).
fn anchor(ctx: &Ctx, app: &str) -> events::Rect {
    let cell = ctx.tray.layout.borrow().cell(app).unwrap_or_default();
    let screen = ctx.to_screen(cell.offset(0, ctx.bar_y()));
    events::Rect {
        x: screen.x,
        y: screen.y,
        w: screen.w.max(0) as u32,
        h: screen.h.max(0) as u32,
    }
}
