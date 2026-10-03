//! The taskbar panel: the "LazyOS" start button, one entry per window and the
//! clock, painted from the shared [`Ctx`] by a single custom node.
//!
//! A press acts at once (the compositor grabs the pointer to the panel until
//! the release, and never moves window focus for a panel press): the start
//! button toggles the menu, an entry activates or minimizes its window
//! according to [`lazyshell::taskbar::Taskbar::click`].

use std::rc::Rc;

use lazyshell::taskbar::{entry_at, Action, START_BUTTON};
use lazyshell::Rect as ShellRect;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, TextStyle};
use xui_core::{Canvas, Control, Dip, MouseButton, Rect};

use super::ctx::{BarHover, Ctx};
use super::menu;
use super::theme::{chrome_look, color, fill_bar};
use xui_core::theme::look;

/// Text size on the bar.
const TEXT: Dip = Dip(12.0);
/// Padding inside an entry, per side.
const ENTRY_PAD: i32 = 8;

/// A taskbar message.
pub enum BarMsg {
    /// The shared state changed; paint again.
    Repaint,
    Move(i32, i32),
    Leave,
    Press(i32, i32),
}

/// The taskbar window's app.
pub struct BarApp {
    ctx: Rc<Ctx>,
    root: Control<BarMsg>,
}

/// The clock's text style: readable on the bar whatever its colour.
fn clock_style(ctx: &Ctx) -> TextStyle {
    let palette = ctx.theme.borrow().palette();
    TextStyle::new(color(uitheme::text_on(palette.taskbar_bg)), TEXT).middle()
}

/// Reserve the width of the widest line the current clock format produces,
/// so the entries never shift as the digits change; `true` when it changed
/// (the entries must then be laid out again).
pub fn measure_clock<M: 'static>(ctx: &Ctx, ui: &Ui<M>) -> bool {
    let format = ctx.theme.borrow().clock_format();
    let widest = lazyshell::clock::widest(format);
    // Measured in screen pixels, kept in design pixels like the bar layout.
    let width = ui.measure_text(widest, &clock_style(ctx), ui.dpi()).width;
    let width = (width + ctx.scale() - 1) / ctx.scale();
    ctx.clock_w.replace(width) != width
}

impl BarApp {
    /// Build the bar: measure the clock so the entries never shift, and
    /// create the painted node that covers the whole panel.
    pub fn build(ctx: Rc<Ctx>, ui: &mut Ui<BarMsg>) -> BarApp {
        measure_clock(&ctx, ui);
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("taskbar node");
        {
            let ctx = Rc::clone(&ctx);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &ctx)));
        }
        root.on_events(|event| match *event {
            Event::MouseMove { x, y, .. } => Some(BarMsg::Move(x, y)),
            Event::MouseLeave => Some(BarMsg::Leave),
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
            } => Some(BarMsg::Press(x, y)),
            _ => None,
        });
        BarApp { ctx, root }
    }

    /// What is under panel-local `(x, y)`.
    fn hover_at(&self, x: i32, y: i32) -> Option<BarHover> {
        if START_BUTTON.contains(x, y) {
            return Some(BarHover::Start);
        }
        entry_at(&self.ctx.entries.borrow(), x, y).map(BarHover::Entry)
    }

    fn set_hover(&self, ui: &Ui<BarMsg>, hover: Option<BarHover>) {
        if self.ctx.bar_hover.replace(hover) != hover {
            ui.invalidate(self.root.id());
        }
    }

    fn press(&self, ui: &Ui<BarMsg>, x: i32, y: i32) {
        match self.hover_at(x, y) {
            Some(BarHover::Start) => menu::toggle(&self.ctx, ui),
            Some(BarHover::Entry(index)) => self.click_entry(index),
            None => {}
        }
    }

    fn click_entry(&self, index: usize) {
        let action = {
            let bar = self.ctx.taskbar.borrow();
            bar.windows()
                .get(index)
                .and_then(|window| bar.click(window.surface))
        };
        let result = match action {
            Some(Action::Activate(surface)) => self.ctx.client.activate_surface(surface),
            Some(Action::Minimize(surface)) => self.ctx.client.minimize_surface(surface),
            None => return,
        };
        if let Err(code) = result {
            self.ctx.note("bar-click", || {
                format!("SHELL:TASKBAR:CLICK:FAIL err={}", -code)
            });
        }
    }
}

impl App for BarApp {
    type Msg = BarMsg;

    fn update(&mut self, msg: BarMsg, ui: &mut Ui<BarMsg>) {
        match msg {
            BarMsg::Repaint => ui.invalidate(self.root.id()),
            // Pointer events are in screen pixels, the bar layout in
            // design pixels.
            BarMsg::Move(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.set_hover(ui, self.hover_at(x, y))
            }
            BarMsg::Leave => self.set_hover(ui, None),
            BarMsg::Press(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.press(ui, x, y)
            }
        }
    }
}

/// A design-pixel shell rectangle as an xui one at scale `s`.
fn rect(r: ShellRect, s: i32) -> Rect {
    Rect::new(r.x * s, r.y * s, (r.x + r.w) * s, (r.y + r.h) * s)
}

/// Paint the whole bar from the shared state. The layout is in design
/// pixels; `s` turns every size into screen pixels (docs/hidpi-plan.md).
fn paint(canvas: &mut dyn Canvas, ctx: &Ctx) {
    let palette = ctx.theme.borrow().palette();
    let s = ctx.scale();
    let deco = chrome_look(ctx.theme.borrow().is_dark());
    let bounds = canvas.bounds();
    fill_bar(canvas, bounds, palette.taskbar_bg, &deco);
    canvas.fill_rect(
        Rect::new(bounds.left, bounds.top, bounds.right, bounds.top + s),
        color(palette.overlay_border),
    );
    let hover = ctx.bar_hover.get();

    let menu_open = ctx.menu_window.borrow().is_some();
    let start_fill = if menu_open || hover == Some(BarHover::Start) {
        palette.taskbar_entry_focus
    } else {
        palette.taskbar_entry
    };
    look::face(canvas, rect(START_BUTTON, s), 4.0 * s as f32, color(start_fill), &deco);
    let start_ink = color(uitheme::text_on(start_fill));
    canvas.draw_text(
        "LazyOS",
        rect(START_BUTTON, s),
        &TextStyle::new(start_ink, TEXT).middle().bold().centered(),
    );

    let bar = ctx.taskbar.borrow();
    let focused = bar.focused();
    for (index, (window, slot)) in bar
        .windows()
        .iter()
        .zip(ctx.entries.borrow().iter())
        .enumerate()
    {
        let Some(slot) = slot else {
            continue;
        };
        let fill = if focused == Some(window.surface) && !window.minimized {
            palette.taskbar_entry_focus
        } else if window.minimized {
            palette.taskbar_entry_min
        } else {
            palette.taskbar_entry
        };
        let area = rect(*slot, s);
        let radius = 3.0 * s as f32;
        look::face(canvas, area, radius, color(fill), &deco);
        if hover == Some(BarHover::Entry(index)) {
            canvas.stroke_rounded_rect(area, radius, color(palette.overlay_border), s as f32);
        }
        // Readable on the entry whatever colour the user picked (#502).
        let ink = if window.minimized {
            uitheme::mix(uitheme::text_on(fill), fill, 1, 2)
        } else {
            uitheme::text_on(fill)
        };
        let label = Rect::new(
            area.left + ENTRY_PAD * s,
            area.top,
            area.right - ENTRY_PAD * s,
            area.bottom,
        );
        canvas.push_clip(label);
        canvas.draw_text(
            &window.title,
            label,
            &TextStyle::new(color(ink), TEXT).middle(),
        );
        canvas.pop_clip();
    }

    let clock = rect(ctx.clock_rect(), s);
    canvas.draw_text(&ctx.clock.borrow(), clock, &clock_style(ctx).centered());
}
