//! The page view: one custom-painted node showing the newest frame WebKit
//! painted, forwarding the mouse, wheel and keyboard to the engine.
//!
//! The frame is the engine's bitmap at the size the engine last laid the page
//! out to; while a resize is in flight it is drawn at its own size from the
//! top-left corner over white, never stretched.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Image, Key, Modifiers, MouseButton};

use crate::app::Msg;
use crate::engine::{Button, Engine, MouseKind};
use crate::keys::{char_keysym, chord_keysym, keysym, modifier_bits};
use crate::mailbox::Frame;

/// xui's wheel unit: 120 for one notch (`WHEEL_DELTA`).
const NOTCH: i32 = 120;

#[derive(Default)]
struct State {
    image: Option<Image>,
    /// The size last told to the engine.
    size: Cell<(i32, i32)>,
    /// The button held since a press in the view, for moves and captures.
    held: Cell<Option<MouseButton>>,
    /// Modifiers of the newest event that carried them.
    mods: Cell<u32>,
}

pub struct PageView {
    control: Control<Msg>,
    state: Rc<RefCell<State>>,
    engine: Rc<dyn Engine>,
}

impl PageView {
    pub fn new(ui: &Ui<Msg>, engine: Rc<dyn Engine>) -> Result<PageView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state: Rc<RefCell<State>> = Rc::default();
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(state) = state.try_borrow() {
                    paint(canvas, &state);
                }
            }));
        }
        {
            let (ui, id) = (ui.clone(), control.id());
            let (state, engine) = (Rc::clone(&state), Rc::clone(&engine));
            control.on_events(move |event| {
                handle(&ui, id, &state, engine.as_ref(), event);
                None
            });
        }
        Ok(PageView {
            control,
            state,
            engine,
        })
    }

    /// Shows a new frame; the previous picture's buffer, to reuse for the next
    /// one, is returned.
    pub fn show(&self, frame: Frame) -> Vec<u8> {
        let mut state = self.state.borrow_mut();
        let spare = state
            .image
            .take()
            .map(Image::into_pixels)
            .unwrap_or_default();
        state.image = Image::from_rgba(frame.width, frame.height, frame.rgba).ok();
        drop(state);
        self.control.invalidate();
        spare
    }

    /// The size of the picture on show.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.state
            .borrow()
            .image
            .as_ref()
            .map(|image| (image.width(), image.height()))
    }

    pub fn focus(&self) {
        self.control.focus();
    }

    pub fn engine(&self) -> &Rc<dyn Engine> {
        &self.engine
    }
}

impl Placeable<Msg> for PageView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    /// The page is laid out at the size the view is given.
    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        let size = (rect.width().max(1), rect.height().max(1));
        let state = self.state.borrow();
        if state.size.replace(size) != size {
            self.engine.resize(size.0, size.1);
        }
    }
}

fn paint(canvas: &mut dyn Canvas, state: &State) {
    let bounds = canvas.bounds();
    canvas.push_clip(bounds);
    canvas.fill_rect(bounds, Color::rgb(255, 255, 255));
    if let Some(image) = &state.image {
        let picture = Rect::new(
            bounds.left,
            bounds.top,
            bounds.left + image.width() as i32,
            bounds.top + image.height() as i32,
        );
        canvas.draw_image(image, picture);
    }
    canvas.pop_clip();
}

fn button_of(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Right,
        MouseButton::X1 | MouseButton::X2 => Button::None,
    }
}

/// Whole notches of a wheel `delta`; a small (smooth) delta still moves once.
fn notches(delta: i16) -> i32 {
    let delta = i32::from(delta);
    match delta / NOTCH {
        0 => delta.signum(),
        whole => whole,
    }
}

/// What a mouse event does to the engine, and to the capture of the pointer.
fn handle(ui: &Ui<Msg>, id: WidgetId, state: &RefCell<State>, engine: &dyn Engine, event: &Event) {
    let Ok(state) = state.try_borrow() else {
        return;
    };
    match *event {
        Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        }
        | Event::MouseDoubleClick {
            x,
            y,
            button,
            modifiers,
        } => {
            state.mods.set(modifier_bits(modifiers));
            ui.focus(id);
            // The backend turns a quick second press into a double click in
            // place of a press; the page counts clicks itself.
            if state.held.get().is_none() {
                state.held.set(Some(button));
                ui.set_capture(id);
            }
            engine.mouse(
                MouseKind::Down,
                button_of(button),
                x,
                y,
                modifier_bits(modifiers),
            );
        }
        Event::MouseUp {
            x,
            y,
            button,
            modifiers,
        } => {
            engine.mouse(
                MouseKind::Up,
                button_of(button),
                x,
                y,
                modifier_bits(modifiers),
            );
            if state.held.get() == Some(button) {
                state.held.set(None);
                ui.release_capture();
            }
        }
        Event::MouseMove { x, y, modifiers } => {
            let held = state.held.get().map_or(Button::None, button_of);
            engine.mouse(MouseKind::Move, held, x, y, modifier_bits(modifiers));
        }
        Event::MouseWheel {
            delta,
            horizontal,
            x,
            y,
            modifiers,
        } => {
            let ticks = notches(delta);
            let (tx, ty) = if horizontal { (ticks, 0) } else { (0, ticks) };
            engine.wheel(x, y, tx, ty, modifier_bits(modifiers));
        }
        Event::CaptureChanged => state.held.set(None),
        Event::SetFocus => engine.focus(true),
        Event::KillFocus => engine.focus(false),
        Event::KeyDown {
            key,
            modifiers,
            system,
            ..
        } => {
            state.mods.set(modifier_bits(modifiers));
            forward_key(engine, true, key, modifiers, system);
        }
        Event::KeyUp {
            key,
            modifiers,
            system,
        } => {
            state.mods.set(modifier_bits(modifiers));
            forward_key(engine, false, key, modifiers, system);
        }
        Event::Char(c) => {
            // Enter, Tab and Backspace came as keys; only text arrives here.
            if !c.is_control() {
                let sym = char_keysym(c);
                let text = c.to_string();
                engine.key(true, sym, &text, state.mods.get());
                engine.key(false, sym, "", state.mods.get());
            }
        }
        _ => {}
    }
}

/// A key press or release as the page gets it: a key that types nothing by
/// its symbol, a Ctrl or Alt chord on a letter by its (lower case) letter.
fn forward_key(engine: &dyn Engine, press: bool, key: Key, modifiers: Modifiers, _system: bool) {
    let bits = modifier_bits(modifiers);
    if let Some(sym) = keysym(key) {
        engine.key(press, sym, "", bits);
    } else if modifiers.ctrl || modifiers.alt {
        if let Some(sym) = chord_keysym(key) {
            engine.key(press, sym, "", bits);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notch_is_one_step_and_a_smooth_delta_still_moves() {
        assert_eq!(notches(120), 1);
        assert_eq!(notches(-120), -1);
        assert_eq!(notches(240), 2);
        assert_eq!(notches(30), 1);
        assert_eq!(notches(-30), -1);
        assert_eq!(notches(0), 0);
    }
}
