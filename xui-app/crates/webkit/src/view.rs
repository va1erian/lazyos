//! The page view: one custom-painted node showing the newest frame WebKit
//! painted, forwarding the mouse, wheel and keyboard to the engine.
//!
//! The frame is the engine's bitmap at the size the engine last laid the page
//! out to; while a resize is in flight it is drawn at its own size from the
//! top-left corner over white, never stretched.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, Cursor, Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Color, Image, Key, Modifiers, MouseButton};

use crate::damage::IRect;
use crate::engine::{Button, Engine, MouseKind};
use crate::events::CursorKind;
use crate::keys::{char_keysym, chord_keysym, keysym, modifier_bits};
use crate::mailbox::{Frame, Update};
use crate::parts::Parts;

/// Whether a key is one of the window's own.
pub type IsShortcut = Rc<dyn Fn(Key, Modifiers) -> bool>;

/// What ties a view to the window that holds it.
pub struct Hooks<M> {
    /// The window's own shortcuts (Ctrl+L, F5, ...): the page never sees them.
    pub is_shortcut: IsShortcut,
    /// The message raised when the page is clicked.
    pub clicked: M,
    /// The message raised for a right click at a point of the view, when the window has a
    /// context menu of its own (the page gets the click as well).
    pub right_click: Option<Rc<dyn Fn(i32, i32) -> M>>,
}

/// xui's wheel unit: 120 for one notch (`WHEEL_DELTA`).
const NOTCH: i32 = 120;

#[derive(Default)]
struct State {
    /// The picture on show: the last whole frame and what was painted over it since.
    picture: Parts,
    /// Microseconds spent drawing the picture, and how many draws, since the window last asked.
    drawn: Cell<(u64, u32)>,
    /// The size last told to the engine.
    size: Cell<(i32, i32)>,
    /// The button held since a press in the view, for moves and captures.
    held: Cell<Option<MouseButton>>,
    /// Modifiers of the newest event that carried them.
    mods: Cell<u32>,
}

pub struct PageView<M: 'static> {
    control: Control<M>,
    state: Rc<RefCell<State>>,
    engine: Rc<dyn Engine>,
}

impl<M: Clone + 'static> PageView<M> {
    pub fn new(ui: &Ui<M>, engine: Rc<dyn Engine>, hooks: Hooks<M>) -> Result<PageView<M>> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state: Rc<RefCell<State>> = Rc::default();
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(state) = state.try_borrow() {
                    let started = Instant::now();
                    paint(canvas, &state);
                    let (us, draws) = state.drawn.get();
                    state
                        .drawn
                        .set((us + started.elapsed().as_micros() as u64, draws + 1));
                }
            }));
        }
        {
            let (ui, id) = (ui.clone(), control.id());
            let (state, engine) = (Rc::clone(&state), Rc::clone(&engine));
            let Hooks {
                is_shortcut,
                clicked,
                right_click,
            } = hooks;
            control.on_events(move |event| {
                handle(&ui, id, &state, engine.as_ref(), &is_shortcut, event);
                match (event, &right_click) {
                    (
                        Event::MouseDown {
                            button: MouseButton::Right,
                            x,
                            y,
                            ..
                        },
                        Some(make),
                    ) => Some(make(*x, *y)),
                    (Event::MouseDown { .. }, _) => Some(clicked.clone()),
                    _ => None,
                }
            });
        }
        Ok(PageView {
            control,
            state,
            engine,
        })
    }

    /// Shows a new whole frame; the previous picture's buffer, to reuse for the next
    /// one, is returned.
    pub fn show(&self, frame: Frame) -> Vec<u8> {
        self.apply(Update {
            full: Some(frame),
            deltas: Vec::new(),
        })
        .spare
    }

    /// Shows what the engine painted: a whole new frame, if there is one, then each delta on top
    /// of the picture. [`Applied`] gives back the buffer the old picture left and whether the
    /// picture can no longer follow the engine (a delta it could not apply, too many parts): the
    /// window then asks the mailbox for a whole frame (`Mailbox::request_full`).
    pub fn apply(&self, update: Update) -> Applied {
        let mut state = self.state.borrow_mut();
        let mut spare = Vec::new();
        let mut resync = false;
        let mut changed = false;
        if let Some(frame) = update.full {
            match Image::from_rgba(frame.width, frame.height, frame.rgba) {
                Ok(image) => spare = state.picture.set_full(image),
                Err(_) => {
                    state.picture.clear();
                    resync = true;
                }
            }
            changed = true;
        }
        for delta in update.deltas {
            if !state.picture.apply(delta) {
                resync = true;
                break;
            }
            changed = true;
        }
        resync |= state.picture.is_fragmented();
        drop(state);
        if changed {
            self.control.invalidate();
        }
        Applied { spare, resync }
    }

    /// Shows the blank page again (the page's process is gone).
    pub fn clear(&self) {
        let had = !self.state.borrow().picture.is_empty();
        self.state.borrow_mut().picture.clear();
        if had {
            self.control.invalidate();
        }
    }

    /// The size of the picture on show.
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        let state = self.state.borrow();
        (!state.picture.is_empty()).then(|| state.picture.size())
    }

    /// How many images the picture on show is made of (for the window's counters).
    pub fn picture_parts(&self) -> usize {
        self.state.borrow().picture.len()
    }

    /// The time spent drawing the picture (microseconds) and the draws, since the last call.
    pub fn take_draw_time(&self) -> (u64, u32) {
        self.state.borrow().drawn.take()
    }

    pub fn focus(&self) {
        self.control.focus();
    }

    pub fn engine(&self) -> &Rc<dyn Engine> {
        &self.engine
    }

    /// The pointer shape over the page, as the page asked for it.
    pub fn set_cursor(&self, kind: CursorKind) {
        self.control
            .ui()
            .set_cursor(self.control.id(), cursor(kind));
    }
}

/// The xui pointer for a page's shape: the compositor has an arrow, a hand, an I-beam and the
/// resize arrows (busy is a window-wide state, and a hidden pointer is not offered).
fn cursor(kind: CursorKind) -> Cursor {
    match kind {
        CursorKind::Hand => Cursor::Hand,
        CursorKind::Text => Cursor::Text,
        CursorKind::ResizeHorizontal => Cursor::SizeHorizontal,
        CursorKind::ResizeVertical => Cursor::SizeVertical,
        CursorKind::ResizeNwSe => Cursor::SizeNwSe,
        CursorKind::ResizeNeSw => Cursor::SizeNeSw,
        CursorKind::Default | CursorKind::Wait | CursorKind::Hidden => Cursor::Default,
    }
}

impl<M: Clone + 'static> Placeable<M> for PageView<M> {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<M>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    /// The page is laid out at the size the view is given.
    fn placed(&self, _ui: &Ui<M>, rect: Rect) {
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
    // White only where the picture does not reach (a window being resized): a frame is opaque and
    // covers the view, and filling under it was a whole extra pass over every repaint.
    let (width, height) = state.picture.size();
    let covered = !state.picture.is_empty()
        && width as i32 >= bounds.width()
        && height as i32 >= bounds.height();
    if !covered {
        canvas.fill_rect(bounds, Color::rgb(255, 255, 255));
    }
    let frame = IRect::new(0, 0, width as i32, height as i32);
    for part in state.picture.parts() {
        let shown = part.visible(&frame);
        if shown.is_empty() {
            continue;
        }
        let whole = IRect::new(
            part.x,
            part.y,
            part.image.width() as i32,
            part.image.height() as i32,
        );
        let at = Rect::new(
            bounds.left + whole.x0,
            bounds.top + whole.y0,
            bounds.left + whole.x1,
            bounds.top + whole.y1,
        );
        if shown == whole {
            canvas.draw_image(&part.image, at);
        } else {
            // Only part of this image is on show: a picture that scrolled, or a patch that was
            // partly covered. Drawn whole, behind a clip.
            canvas.push_clip(Rect::new(
                bounds.left + shown.x0,
                bounds.top + shown.y0,
                bounds.left + shown.x1,
                bounds.top + shown.y1,
            ));
            canvas.draw_image(&part.image, at);
            canvas.pop_clip();
        }
    }
    canvas.pop_clip();
}

/// What [`PageView::apply`] leaves for the window to do.
#[derive(Debug, Default)]
pub struct Applied {
    /// The previous whole picture's buffer, to be filled again by the next frame.
    pub spare: Vec<u8>,
    /// The picture cannot follow the engine any more: ask for a whole frame.
    pub resync: bool,
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
fn handle<M: 'static>(
    ui: &Ui<M>,
    id: WidgetId,
    state: &RefCell<State>,
    engine: &dyn Engine,
    is_shortcut: &IsShortcut,
    event: &Event,
) {
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
