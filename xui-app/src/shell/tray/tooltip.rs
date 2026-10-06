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
/// The panel's width, one line's height and the padding (design pixels).
const WIDTH: i32 = 240;
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
    let target = ctx.tray.hover.get().and_then(|(cell, since)| {
        let rested = sys::clock_ticks().saturating_sub(since);
        if rested >= SHOWN_TICKS {
            // The bar does not always hear the pointer leave for another
            // surface, so a tooltip never outstays a few seconds.
            ctx.tray.hover.set(None);
            return None;
        }
        let rested = rested >= HOVER_TICKS;
        let layout = ctx.tray.layout.borrow();
        rested.then(|| layout.cells.get(cell).map(|c| (c.app.clone(), c.rect)))?
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
    let x = (cell.x + cell.w / 2 - WIDTH / 2).clamp(0, (ctx.screen.0 - WIDTH).max(0));
    let y = ctx.screen.1 - BAR_H - GAP - height;
    let s = ctx.scale();
    ctx.backend
        .set_next_role(SurfaceRole::Panel { x: x * s, y: y * s });
    let spec = PlatformSpec::new("LazyOS").size(Dip(WIDTH as f32), Dip(height as f32));
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
