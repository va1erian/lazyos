//! The taskbar panel: the "LazyOS" start button, one entry per window and the
//! clock, painted from the shared [`Ctx`] by a single custom node.
//!
//! A press acts at once (the compositor grabs the pointer to the panel until
//! the release, and never moves window focus for a panel press): the start
//! button toggles the menu, an entry activates or minimizes its window
//! according to [`lazyshell::taskbar::Taskbar::click`]. The "Log out"
//! button left of the clock (issue #623) takes two presses: the first turns it
//! into "Log out?", the second asks `logind` to end the session; leaving the
//! bar disarms it. Serial: `SHELL:LOGOUT:ARMED`, `SHELL:LOGOUT:REQUEST
//! session=<id>` or `SHELL:LOGOUT:FAIL errno=<e>`.

use std::rc::Rc;

use lazyshell::taskbar::{entry_at, Action, START_BUTTON};
use lazyshell::Rect as ShellRect;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, TextStyle};
use xui_core::{Canvas, Control, Dip, MouseButton, Rect, Rgba};

use super::ctx::{BarHover, Ctx};
use super::menu;
use super::theme::{chrome_look, color, fill_bar};
use super::tray::{self, input::Input};
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
    /// A right-button press (the tray's secondary click).
    Secondary(i32, i32),
    /// The wheel rolled `delta` notches at `(x, y)`.
    Wheel(i32, i32, i32),
}

/// The taskbar window's app.
pub struct BarApp {
    ctx: Rc<Ctx>,
    root: Control<BarMsg>,
}

/// The clock's text style: readable on the bar whatever its colour, and
/// secondary on the dark bar when dimming it still leaves it readable (a
/// mid-tone taskbar override keeps the full-contrast ink).
fn clock_style(ctx: &Ctx) -> TextStyle {
    let theme = ctx.theme.borrow();
    let palette = theme.palette();
    let ink = uitheme::text_on(palette.taskbar_bg);
    let dimmed = uitheme::mix(ink, palette.taskbar_bg, 1, 4);
    let ink = if theme.is_dark() && readable(dimmed, palette.taskbar_bg) {
        dimmed
    } else {
        ink
    };
    TextStyle::new(color(ink), TEXT).middle()
}

/// Whether `ink` on `background` reaches the WCAG AA 4.5:1 text contrast.
fn readable(ink: u32, background: u32) -> bool {
    // 0.05 in relative luminance's 0..=65535 scale.
    const FLARE: u64 = 3277;
    let (a, b) = (
        u64::from(uitheme::relative_luminance(ink)) + FLARE,
        u64::from(uitheme::relative_luminance(background)) + FLARE,
    );
    a.max(b) * 10 >= a.min(b) * 45
}

/// The focused entry's pill colour on a dark bar: the accent halfway to the
/// bar, so the pill reads as a tint of it.
fn focus_tint(palette: &uitheme::Palette) -> u32 {
    uitheme::mix(palette.taskbar_entry_focus, palette.taskbar_bg, 1, 2)
}

/// A dark bar's window entry (the Midnight mockup): no box at rest, a faint
/// one on hover, and for the focused window an accent-tinted glossy pill
/// with an accent underline.
fn paint_entry(
    canvas: &mut dyn Canvas,
    area: Rect,
    s: i32,
    palette: &uitheme::Palette,
    deco: &xui_core::Theme,
    focused: bool,
    hovered: bool,
) {
    if focused {
        let tint = focus_tint(palette);
        let radius = 6.0 * s as f32;
        let edge = color(uitheme::mix(tint, 0xFF_FF_FF, 1, 8));
        look::face(canvas, area, radius, color(tint), deco);
        canvas.stroke_rounded_rect(area, radius, edge, s as f32);
        let accent = uitheme::mix(palette.taskbar_entry_focus, 0xFF_FF_FF, 1, 3);
        let line = Rect::new(
            area.left + 6 * s,
            area.bottom - 2 * s,
            area.right - 6 * s,
            area.bottom,
        );
        canvas.fill_rounded_rect(line, s as f32, color(accent));
    } else if hovered {
        canvas.fill_rect_rgba(area.shrink(s), Rgba::with_alpha(0xFF, 0xFF, 0xFF, 0x12));
    }
}

/// A dark bar entry's text colour: readable on the focused pill's tint
/// whatever the accent, dimmed when minimised.
fn entry_ink(palette: &uitheme::Palette, focused: bool, minimized: bool) -> u32 {
    if focused {
        uitheme::text_on(focus_tint(palette))
    } else if minimized {
        uitheme::mix(palette.overlay_text, palette.taskbar_bg, 1, 2)
    } else {
        palette.overlay_text
    }
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
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Right,
                ..
            } => Some(BarMsg::Secondary(x, y)),
            Event::MouseWheel {
                x,
                y,
                delta,
                horizontal: false,
                ..
            } => Some(BarMsg::Wheel(x, y, notches(delta))),
            _ => None,
        });
        BarApp { ctx, root }
    }

    /// What is under panel-local `(x, y)`.
    fn hover_at(&self, x: i32, y: i32) -> Option<BarHover> {
        if START_BUTTON.contains(x, y) {
            return Some(BarHover::Start);
        }
        if self.ctx.logout_rect().contains(x, y) {
            return Some(BarHover::Logout);
        }
        if let Some(hit) = tray::input::hit(&self.ctx, x, y) {
            return Some(hit);
        }
        entry_at(&self.ctx.entries.borrow(), x, y).map(BarHover::Entry)
    }

    fn set_hover(&self, ui: &Ui<BarMsg>, hover: Option<BarHover>) {
        if self.ctx.bar_hover.replace(hover) != hover {
            tray::input::hovered(&self.ctx, hover);
            ui.invalidate(self.root.id());
        }
    }

    fn press(&self, ui: &Ui<BarMsg>, x: i32, y: i32) {
        match self.hover_at(x, y) {
            Some(BarHover::Start) => menu::toggle(&self.ctx, ui),
            Some(BarHover::Entry(index)) => self.click_entry(index),
            Some(BarHover::Logout) => self.press_logout(ui),
            Some(BarHover::Tray(cell)) => tray::input::on_cell(&self.ctx, ui, cell, Input::Primary),
            // The overflow panel arrives with docs/tray-plan.md stage T5.
            Some(BarHover::Chevron) | None => {}
        }
    }

    /// A right press or a wheel roll: only tray cells take them.
    fn tray_input(&self, ui: &Ui<BarMsg>, x: i32, y: i32, input: Input) {
        if let Some(BarHover::Tray(cell)) = self.hover_at(x, y) {
            tray::input::on_cell(&self.ctx, ui, cell, input);
        }
    }

    /// The first press arms the button, the second logs out.
    fn press_logout(&self, ui: &Ui<BarMsg>) {
        if !self.ctx.logout_armed.replace(true) {
            println!("SHELL:LOGOUT:ARMED");
            ui.invalidate(self.root.id());
            return;
        }
        self.ctx.logout_armed.set(false);
        match super::services::logout() {
            Ok(session) => println!("SHELL:LOGOUT:REQUEST session={session}"),
            Err(code) => println!("SHELL:LOGOUT:FAIL errno={}", -code),
        }
        ui.invalidate(self.root.id());
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
            BarMsg::Leave => {
                if self.ctx.logout_armed.replace(false) {
                    ui.invalidate(self.root.id());
                }
                self.set_hover(ui, None)
            }
            BarMsg::Press(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.press(ui, x, y)
            }
            BarMsg::Secondary(x, y) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.tray_input(ui, x, y, Input::Secondary)
            }
            BarMsg::Wheel(x, y, delta) => {
                let (x, y) = self.ctx.to_design(x, y);
                self.tray_input(ui, x, y, Input::Wheel(delta))
            }
        }
    }
}

/// A wheel `delta` in notches: the backend reports `WHEEL_DELTA` (120) per
/// notch, and a partial roll still counts as one.
fn notches(delta: i16) -> i32 {
    let delta = i32::from(delta);
    delta.signum() * (delta.abs() / 120).max(1)
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
    let mut deco = chrome_look(ctx.theme.borrow().is_dark());
    deco.accent = color(palette.taskbar_entry_focus);
    let fancy = look::decorated(&deco);
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
    if fancy {
        // The mockup's start button: always the accent, glossy, with a halo.
        let button = rect(START_BUTTON, s).shrink(2 * s);
        look::halo(canvas, button, 6.0 * s as f32, &deco);
        let fill = if menu_open || hover == Some(BarHover::Start) {
            uitheme::mix(palette.taskbar_entry_focus, 0xFF_FF_FF, 1, 6)
        } else {
            palette.taskbar_entry_focus
        };
        look::face(canvas, button, 6.0 * s as f32, color(fill), &deco);
    } else {
        look::face(
            canvas,
            rect(START_BUTTON, s),
            4.0 * s as f32,
            color(start_fill),
            &deco,
        );
    }
    let start_fill = if fancy {
        palette.taskbar_entry_focus
    } else {
        start_fill
    };
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
        let is_focused = focused == Some(window.surface) && !window.minimized;
        let hovered = hover == Some(BarHover::Entry(index));
        let ink = if fancy {
            paint_entry(canvas, area, s, &palette, &deco, is_focused, hovered);
            entry_ink(&palette, is_focused, window.minimized)
        } else {
            let radius = 3.0 * s as f32;
            look::face(canvas, area, radius, color(fill), &deco);
            if hovered {
                canvas.stroke_rounded_rect(area, radius, color(palette.overlay_border), s as f32);
            }
            // Readable on the entry whatever colour the user picked (#502).
            if window.minimized {
                uitheme::mix(uitheme::text_on(fill), fill, 1, 2)
            } else {
                uitheme::text_on(fill)
            }
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

    paint_logout(canvas, ctx, &palette, &deco, hover == Some(BarHover::Logout));
    let tray_hover = match hover {
        Some(BarHover::Tray(cell)) => Some(cell),
        _ => None,
    };
    tray::paint::paint(canvas, ctx, tray_hover, hover == Some(BarHover::Chevron));

    let clock = rect(ctx.clock_rect(), s);
    canvas.draw_text(&ctx.clock.borrow(), clock, &clock_style(ctx).centered());
}

/// The "Log out" button: an entry-like face, highlighted while hovered or
/// armed (then it reads "Log out?").
fn paint_logout(
    canvas: &mut dyn Canvas,
    ctx: &Ctx,
    palette: &uitheme::Palette,
    deco: &xui_core::Theme,
    hovered: bool,
) {
    let s = ctx.scale();
    let armed = ctx.logout_armed.get();
    let fill = if armed || hovered {
        palette.taskbar_entry_focus
    } else {
        palette.taskbar_entry
    };
    let area = rect(ctx.logout_rect(), s);
    look::face(canvas, area, 3.0 * s as f32, color(fill), deco);
    let text = if armed { "Log out?" } else { "Log out" };
    let ink = color(uitheme::text_on(fill));
    canvas.draw_text(text, area, &TextStyle::new(ink, TEXT).middle().centered());
}
