//! The tray tooltip (docs/tray-plan.md section 7.2): resting the pointer on
//! a cell for half a second opens a small panel above it, headed by the
//! app's verified registry name (never app text) with the app's tooltip
//! below. Moving off the cell closes it.
//!
//! Serial: `SHELL:TRAY:TOOLTIP app=<id>`.

use std::rc::Rc;

use lazyshell::taskbar::BAR_H;
use xui_core::app::{App, Ui, WindowHandle};
use xui_core::backend::{NodeKind, NodeSpec, PlatformSpec, TextStyle};
use xui_core::{Canvas, Control, Dip, Rect};

use super::super::ctx::Ctx;
use super::super::theme::{chrome_look, color, fill_bar};
use crate::client_window::SurfaceRole;
use crate::sys;

/// Ticks (100 Hz) the pointer rests before the tooltip opens.
const HOVER_TICKS: u64 = 50;
/// Ticks after which the tooltip closes by itself (4 s of showing).
const SHOWN_TICKS: u64 = HOVER_TICKS + 400;
/// The panel's width (design pixels): at least [`WIDTH`], wider for a long
/// tooltip (the Wi-Fi applet's names the network, signal, band and address)
/// up to [`MAX_WIDTH`], estimated from the text since a panel is sized
/// before it is drawn.
const WIDTH: i32 = 240;
const MAX_WIDTH: i32 = 520;
/// Design pixels one character of the 12 dp text takes, rounded up.
const CHAR_W: i32 = 6;
/// One line's height and the padding (design pixels).
const LINE: i32 = 18;
const PAD: i32 = 6;
/// Gap between the panel and the bar.
const GAP: i32 = 4;

/// The open tooltip.
pub struct Open {
    app: String,
    handle: WindowHandle<()>,
}

/// What the panel shows.
struct Text {
    name: String,
    tooltip: String,
}

/// Open or close the tooltip from the hover state.
pub fn pump<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>) {
    // An open menu sits where the tooltip would, and a panel created later
    // is on top: it would take the menu's clicks.
    if !ctx.tray.menus.borrow().is_empty() {
        close(ctx);
        return;
    }
    let hover = ctx.tray.hover.borrow().clone();
    let target = hover.and_then(|(app, since)| {
        let rested = sys::clock_ticks().saturating_sub(since);
        if rested >= SHOWN_TICKS {
            // The bar does not always hear the pointer leave for another
            // surface, so a tooltip never outstays a few seconds.
            *ctx.tray.hover.borrow_mut() = None;
            return None;
        }
        // An app that left the bar (or moved off it) shows no tooltip.
        let cell = ctx.tray.layout.borrow().cell(&app)?;
        (rested >= HOVER_TICKS).then_some((app, cell))
    });
    let open_app = ctx
        .tray
        .tooltip
        .borrow()
        .as_ref()
        .map(|open| open.app.clone());
    match (target, open_app) {
        (Some((app, _)), Some(open)) if app == open => {}
        (Some((app, cell)), _) => {
            close(ctx);
            open(ctx, ui, app, cell);
        }
        (None, Some(_)) => close(ctx),
        (None, None) => {}
    }
}

/// Close the tooltip, if open.
pub fn close(ctx: &Ctx) {
    if let Some(open) = ctx.tray.tooltip.borrow_mut().take() {
        open.handle.close();
    }
}

fn open<M: 'static>(ctx: &Rc<Ctx>, ui: &Ui<M>, app: String, cell: lazyshell::Rect) {
    let text = Text {
        name: ctx.tray.name(&app),
        tooltip: ctx
            .tray
            .model
            .borrow()
            .get(&app)
            .map(|entry| entry.tooltip().to_owned())
            .unwrap_or_default(),
    };
    let lines = if text.tooltip.is_empty() { 1 } else { 2 };
    let height = lines * LINE + 2 * PAD;
    let width = panel_width(&text.tooltip);
    let x = (cell.x + cell.w / 2 - width / 2).clamp(0, (ctx.screen.0 - width).max(0));
    let y = ctx.screen.1 - BAR_H - GAP - height;
    let s = ctx.scale();
    ctx.backend
        .set_next_role(SurfaceRole::Panel { x: x * s, y: y * s });
    let spec = PlatformSpec::new("LazyOS").size(Dip(width as f32), Dip(height as f32));
    let built = Rc::clone(ctx);
    match ui.open_window(spec, move |ui| Tip::build(built, text, ui)) {
        Ok(handle) => {
            println!("SHELL:TRAY:TOOLTIP app={app}");
            *ctx.tray.tooltip.borrow_mut() = Some(Open { app, handle });
        }
        Err(error) => ctx.note("tray-tooltip", || {
            format!("SHELL:TRAY:TOOLTIP:FAIL {error}")
        }),
    }
}

/// The panel's width for `tooltip`.
fn panel_width(tooltip: &str) -> i32 {
    let chars = i32::try_from(tooltip.chars().count()).unwrap_or(i32::MAX / CHAR_W);
    (chars.saturating_mul(CHAR_W) + 2 * PAD).clamp(WIDTH, MAX_WIDTH)
}

/// The tooltip panel's app. It holds its painted node: dropping the
/// control would leave the panel blank.
struct Tip {
    _root: Control<()>,
}

impl Tip {
    fn build(ctx: Rc<Ctx>, text: Text, ui: &mut Ui<()>) -> Tip {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("tooltip node");
        root.set_painter(Rc::new(move |canvas| paint(canvas, &ctx, &text)));
        Tip { _root: root }
    }
}

impl App for Tip {
    type Msg = ();

    fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
}

fn paint(canvas: &mut dyn Canvas, ctx: &Ctx, text: &Text) {
    let palette = ctx.theme.borrow().palette();
    let s = ctx.scale();
    let deco = chrome_look(ctx.theme.borrow().is_dark());
    let bounds = canvas.bounds();
    fill_bar(canvas, bounds, palette.overlay_bg, &deco);
    canvas.stroke_rect(bounds, color(palette.overlay_border), s as f32);
    let ink = color(palette.overlay_text);
    let line = |index: i32| {
        let top = bounds.top + (PAD + index * LINE) * s;
        Rect::new(
            bounds.left + PAD * s,
            top,
            bounds.right - PAD * s,
            top + LINE * s,
        )
    };
    canvas.draw_text(
        &text.name,
        line(0),
        &TextStyle::new(ink, Dip(12.0)).middle().bold(),
    );
    if !text.tooltip.is_empty() {
        canvas.push_clip(line(1));
        canvas.draw_text(
            &text.tooltip,
            line(1),
            &TextStyle::new(ink, Dip(12.0)).middle(),
        );
        canvas.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_tooltip_keeps_the_usual_width_and_a_long_one_grows_to_a_cap() {
        assert_eq!(panel_width(""), WIDTH);
        assert_eq!(panel_width("Wi-Fi: not connected"), WIDTH);
        let long =
            "Wi-Fi: connected to LazyOpen, signal excellent (-40 dBm), 2.4 GHz, 192.168.77.100";
        let width = panel_width(long);
        assert!(width > WIDTH && width <= MAX_WIDTH, "{width}");
        assert!(
            width >= long.chars().count() as i32 * 5,
            "room for every character"
        );
        assert_eq!(panel_width(&"x".repeat(10_000)), MAX_WIDTH);
    }
}
