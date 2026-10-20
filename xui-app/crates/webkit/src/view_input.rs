//! The page view's input: the mouse, the wheel and the keyboard as the engine gets them, and the
//! timer that gives up on a picture shown ahead of the page (`view.rs` is the picture).

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Instant;

use xui_core::app::Ui;
use xui_core::backend::{Event, WidgetId};
use xui_core::widget::Control;
use xui_core::{Key, Modifiers, MouseButton};

use crate::engine::{Button, Engine, MouseKind};
use crate::keys::{char_keysym, chord_keysym, keysym, modifier_bits};
use crate::speculate;
use crate::view::{IsShortcut, State};

/// xui's wheel unit: 120 for one notch (`WHEEL_DELTA`).
const NOTCH: i32 = 120;
/// How often a picture shown ahead of the page is checked for being given up on.
const SETTLE_TICK_MS: u32 = 100;
/// The glide's frame (`glide.rs`): about 60 a second, while the picture moves.
const GLIDE_FRAME_MS: u32 = 16;

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

/// Starts the timer that gives up on a picture shown ahead of the page (`speculate.rs`). It stops
/// itself once nothing is ahead, so an idle page costs no wakeups.
pub(crate) fn arm_settle<M: Clone + 'static>(
    control: &Weak<Control<M>>,
    state: &Rc<RefCell<State>>,
) {
    let (Some(strong), Ok(held)) = (control.upgrade(), state.try_borrow()) else {
        return;
    };
    if held.settle.get().is_some() {
        return;
    }
    let (weak, state) = (control.clone(), Rc::clone(state));
    let held_state = Rc::clone(&state);
    let timer = strong.set_timer(SETTLE_TICK_MS, move || {
        let control = weak.upgrade()?;
        let state = held_state.try_borrow().ok()?;
        if state.spec.borrow_mut().tick(speculate::now_ms()) {
            // What was ahead was given up on: the picture glides back.
            control.invalidate();
            drop(state);
            arm_glide(&weak, &held_state);
            return None;
        }
        if !state.spec.borrow().is_ahead() {
            if let Some(timer) = state.settle.take() {
                control.kill_timer(timer);
            }
        }
        None
    });
    held.settle.set(timer);
}

/// Starts the glide's frame timer (`glide.rs`) unless it runs: each frame moves the picture toward where
/// the wheel asked and redraws it; the timer stops itself once the picture is there.
pub(crate) fn arm_glide<M: Clone + 'static>(
    control: &Weak<Control<M>>,
    state: &Rc<RefCell<State>>,
) {
    let (Some(strong), Ok(held)) = (control.upgrade(), state.try_borrow()) else {
        return;
    };
    if held.glide_timer.get().is_some() || !held.glide.borrow().moving(held.target()) {
        return;
    }
    let (weak, state) = (control.clone(), Rc::clone(state));
    let timer = strong.set_timer(GLIDE_FRAME_MS, move || {
        let control = weak.upgrade()?;
        let state = state.try_borrow().ok()?;
        let target = state.target();
        if state.glide.borrow_mut().step(target, speculate::now_us()) {
            control.invalidate();
        }
        if !state.glide.borrow().moving(target) {
            if let Some(timer) = state.glide_timer.take() {
                control.kill_timer(timer);
            }
        }
        None
    });
    held.glide_timer.set(timer);
}

/// What a mouse event does to the engine, and to the capture of the pointer.
pub(crate) fn handle<M: 'static>(
    ui: &Ui<M>,
    id: WidgetId,
    state: &RefCell<State>,
    engine: &dyn Engine,
    is_shortcut: &IsShortcut,
    arm: &dyn Fn(),
    event: &Event,
) {
    let Ok(state) = state.try_borrow() else {
        return;
    };
    // A click or a key may move the page (a link to an anchor, Page Down): that move is not the
    // wheel's answer, and jumps.
    if matches!(*event, Event::MouseDown { .. } | Event::KeyDown { .. }) {
        state.wheel_asked.set(None);
    }
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
            let before = state.target();
            let (tx, ty) = if horizontal { (ticks, 0) } else { (0, ticks) };
            engine.wheel(x, y, tx, ty, modifier_bits(modifiers));
            // A plain vertical wheel scrolls the page: show the picture moved at once, not when the
            // page has answered, if pictures are shown ahead (a modified wheel zooms or scrolls
            // sideways). Otherwise the view moves with the page's answer (`View::apply`).
            let plain = !horizontal && modifier_bits(modifiers) == 0;
            let now_ms = speculate::now_ms();
            if plain && !speculate::ahead() {
                // What the wheel asks for since the page last answered (a notch down is a negative
                // tick and a positive move of the page), to glide to that answer and only to it.
                let asked = state
                    .wheel_asked
                    .get()
                    .filter(|(at, _)| now_ms.saturating_sub(*at) <= speculate::ANSWER_MS)
                    .map_or(0, |(_, px)| px);
                let step =
                    -ticks.clamp(-speculate::MAX_TICKS, speculate::MAX_TICKS) * speculate::LINE_PX;
                state.wheel_asked.set(Some((now_ms, asked + step)));
            }
            let moved = plain && speculate::ahead() && state.spec.borrow_mut().wheel(ticks, now_ms);
            // The scroll meter counts a notch sent with nothing shown ahead as the start of a burst too.
            state
                .smooth
                .borrow_mut()
                .wheel(now_ms, moved || (plain && !speculate::ahead()));
            if moved {
                if state.spec.borrow().is_ahead() && state.wheel_at.get().is_none() {
                    state.wheel_at.set(Some(Instant::now()));
                }
                // The picture glides there (`glide.rs`): the first frame of the slide now.
                let now = speculate::now_us();
                state.glide.borrow_mut().start(before, now);
                state.glide.borrow_mut().step(state.target(), now + 4_000);
                ui.invalidate(id);
                arm();
            }
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
            forward_key(engine, is_shortcut, true, key, modifiers, system);
        }
        Event::KeyUp {
            key,
            modifiers,
            system,
        } => {
            state.mods.set(modifier_bits(modifiers));
            forward_key(engine, is_shortcut, false, key, modifiers, system);
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
fn forward_key(
    engine: &dyn Engine,
    is_shortcut: &IsShortcut,
    press: bool,
    key: Key,
    modifiers: Modifiers,
    _system: bool,
) {
    // The window's own shortcuts (Ctrl+L, F5, Alt+Left...) are the window's, not the page's.
    if is_shortcut(key, modifiers) {
        return;
    }
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
