//! The mouse: selecting with it (and autoscrolling while a drag is past the
//! edge), and reporting it to a program that asked (`?1000`/`?1002`/`?1003`).
//! Holding Shift always selects, whatever the program asked for.

use lazyterm::input::{mouse_report, Action, Button, Mods, MouseMode};
use lazyterm::SelectMode;
use xui_app::sys::monotonic_ns;
use xui_core::app::Ui;
use xui_core::{Modifiers, MouseButton};

use super::geometry::{point_at, screen_cell, Edge};
use super::view::Layout;
use super::scroll::{wheel_arrows, wheel_move, Move};
use super::{Msg, Terminal};

/// How often a drag past the edge scrolls (milliseconds): the only timer the
/// Terminal runs besides its 1 s safety net, and only while dragging.
const AUTOSCROLL_MILLIS: u32 = 60;

/// What the pointer did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Move,
    /// The second press of a double-click (it replaces that `Down`).
    Double(MouseButton),
}

/// A pointer event on the Terminal's node (node pixels).
#[derive(Clone, Copy, Debug)]
pub struct MouseMsg {
    pub kind: MouseKind,
    pub x: i32,
    pub y: i32,
    pub mods: Modifiers,
}

/// The reported button; the side buttons are not reported.
fn button(button: MouseButton) -> Option<Button> {
    match button {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        MouseButton::Right => Some(Button::Right),
        _ => None,
    }
}

fn mods(m: Modifiers) -> Mods {
    Mods {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.ctrl,
    }
}

impl Terminal {
    /// Whether the program gets the mouse: it asked for it, and Shift is not
    /// held.
    fn reporting(&self, m: Modifiers) -> bool {
        !m.shift && self.term.borrow().mouse_mode() != MouseMode::Off
    }

    /// Handle a pointer event.
    pub(super) fn mouse(&mut self, ui: &mut Ui<Msg>, m: MouseMsg) {
        if self.pending_text.is_some() {
            self.confirm_click(ui, &m);
            return;
        }
        self.last_mouse = (m.x, m.y);
        if self.reporting(m.mods) && !self.drag {
            self.report_mouse(ui, &m);
            return;
        }
        match m.kind {
            MouseKind::Down(MouseButton::Left) => self.press(ui, &m, false),
            MouseKind::Double(MouseButton::Left) => self.press(ui, &m, true),
            MouseKind::Down(MouseButton::Middle) => self.paste(ui),
            MouseKind::Move if self.drag => self.drag_to(ui),
            MouseKind::Up(MouseButton::Left) if self.drag => self.release(ui),
            _ => {}
        }
    }

    /// A wheel roll at (`x`, `y`): reported to the program that asked for the
    /// mouse, else it scrolls the view (or sends arrows to a full-screen
    /// program with no mouse mode).
    pub(super) fn wheel(&mut self, ui: &mut Ui<Msg>, delta: i16, m: Modifiers, at: (i32, i32)) {
        // Ctrl+wheel zooms this window; the program never sees it.
        if m.ctrl && !m.alt {
            let steps = (i32::from(delta) / 120).clamp(-3, 3);
            let steps = if steps == 0 { i32::from(delta.signum()) } else { steps };
            return self.zoom_by(ui, Some(steps));
        }
        if self.reporting(m) {
            let which = if delta > 0 { Button::WheelUp } else { Button::WheelDown };
            for _ in 0..(i32::from(delta) / 120).unsigned_abs().clamp(1, 8) {
                self.send_report(Some(which), Action::Press, m, at);
            }
        } else if self.term.borrow().on_alternate_screen() {
            let arrows = wheel_arrows(delta);
            self.send(&arrows);
        } else if let Some(mv) = wheel_move(delta, m.shift, self.page()) {
            self.scroll(ui, mv);
        }
    }

    /// Send the program a mouse report for a button (or motion) at pixel `at`.
    fn send_report(&mut self, which: Option<Button>, action: Action, m: Modifiers, at: (i32, i32)) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let bytes = {
            let term = self.term.borrow();
            let (col, row) = screen_cell(&layout, &term, at.0, at.1);
            mouse_report(term.mouse_mode(), term.mouse_sgr(), which, action, mods(m), col, row)
        };
        if let Some(bytes) = bytes {
            self.send(&bytes);
        }
    }

    fn report_mouse(&mut self, ui: &mut Ui<Msg>, m: &MouseMsg) {
        let at = (m.x, m.y);
        match m.kind {
            MouseKind::Down(b) | MouseKind::Double(b) => {
                let Some(b) = button(b) else { return };
                self.held = Some(b);
                ui.set_capture(self.root.id());
                self.send_report(Some(b), Action::Press, m.mods, at);
            }
            MouseKind::Up(b) => {
                let Some(b) = button(b) else { return };
                self.held = None;
                ui.release_capture();
                self.send_report(Some(b), Action::Release, m.mods, at);
            }
            MouseKind::Move => self.send_report(self.held, Action::Motion, m.mods, at),
        }
    }

    /// A left press: start (or, with Shift, extend) a selection.
    fn press(&mut self, ui: &mut Ui<Msg>, m: &MouseMsg, double: bool) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let now = monotonic_ns();
        let triple = !double && self.clicks.is_triple(now, m.x, m.y);
        {
            let mut term = self.term.borrow_mut();
            let (at, _) = point_at(&layout, &term, self.offset.get(), m.x, m.y);
            if double {
                self.clicks.double(now, m.x, m.y);
                term.select_begin(SelectMode::Word, at);
            } else if triple {
                term.select_begin(SelectMode::Line, at);
            } else if m.mods.shift {
                term.select_extend(at);
            } else if m.mods.alt {
                term.select_begin(SelectMode::Block, at);
            } else {
                term.select_begin(SelectMode::Char, at);
            }
        }
        self.drag = true;
        ui.set_capture(self.root.id());
        self.refresh(ui);
    }

    /// The pointer moved with the button down: grow the selection, and
    /// autoscroll while it is past the top or bottom edge.
    fn drag_to(&mut self, ui: &mut Ui<Msg>) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let edge = {
            let mut term = self.term.borrow_mut();
            let (at, edge) = point_at(&layout, &term, self.offset.get(), self.last_mouse.0, self.last_mouse.1);
            term.select_update(at);
            edge
        };
        self.set_autoscroll(ui, edge != Edge::Inside);
        self.refresh(ui);
    }

    /// The autoscroll timer fired: one line towards the edge the pointer is
    /// past, and the selection follows.
    pub(super) fn autoscroll(&mut self, ui: &mut Ui<Msg>) {
        let Some(layout) = self.layout.get().filter(|_| self.drag) else {
            self.set_autoscroll(ui, false);
            return;
        };
        let edge = {
            let term = self.term.borrow();
            point_at(&layout, &term, self.offset.get(), self.last_mouse.0, self.last_mouse.1).1
        };
        match edge {
            Edge::Above => self.scroll(ui, Move::Back(1)),
            Edge::Below => self.scroll(ui, Move::Forward(1)),
            Edge::Inside => {}
        }
        self.drag_to(ui);
    }

    /// Run the autoscroll timer while it is needed, and not otherwise: an
    /// idle Terminal costs no frames.
    fn set_autoscroll(&mut self, ui: &mut Ui<Msg>, on: bool) {
        match (on, self.auto.get()) {
            (true, None) => self.auto.set(Some(ui.set_timer(AUTOSCROLL_MILLIS))),
            (false, Some(id)) => {
                ui.kill_timer(id);
                self.auto.set(None);
            }
            _ => {}
        }
    }

    /// The button went up: the selection stays, the drag ends.
    fn release(&mut self, ui: &mut Ui<Msg>) {
        self.drag = false;
        self.set_autoscroll(ui, false);
        ui.release_capture();
        let selected = self.term.borrow().selection_text();
        if let Some(text) = &selected {
            println!("TERM:SELECT:{}", text.len());
            if self.conf.settings.copy_on_select {
                self.copy(ui);
            }
        }
    }
}

pub(super) fn mouse(kind: MouseKind, x: i32, y: i32, mods: Modifiers) -> Msg {
    Msg::Mouse(MouseMsg { kind, x, y, mods })
}

/// Tell session scripts where the grid is (`UI:WIDGET`, with the probe on):
/// its top-left is the origin the drags in `term_select_copy.json` use.
pub(super) fn probe_grid(layout: &Layout) {
    let cols = super::COLS as i32;
    let width = (layout.width - 2 * layout.pad).min(cols * layout.cell_w);
    xui_app::probe::widget(
        "Terminal",
        "grid",
        layout.pad,
        layout.pad,
        width,
        layout.visible as i32 * layout.line_h,
    );
}
