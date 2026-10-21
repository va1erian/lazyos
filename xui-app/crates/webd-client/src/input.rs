//! Input and visibility: oneway messages to the view. Nothing here waits for webd or can fail in a
//! way the caller could act on (a view that is gone is reported by [`crate::LightWebView::poll`]
//! and the next call that has an answer), so none returns a result.

use messenger_generated::os_lazy_webd_light_v1 as wire;

use crate::transport::Transport;
use crate::view::LightWebView;

/// `Mouse.kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Move,
    Press,
    Release,
}

impl MouseKind {
    fn wire(self) -> u32 {
        match self {
            MouseKind::Move => wire::MOUSE_KIND_MOVE,
            MouseKind::Press => wire::MOUSE_KIND_PRESS,
            MouseKind::Release => wire::MOUSE_KIND_RELEASE,
        }
    }
}

/// Modifier bits of `Mouse`, `Wheel` and `Key`.
pub mod mods {
    pub const SHIFT: u32 = 1;
    pub const CTRL: u32 = 2;
    pub const ALT: u32 = 4;
    pub const META: u32 = 8;
}

impl<T: Transport> LightWebView<T> {
    /// A view that is not visible gets no paints (its page still runs). Remembered across
    /// [`LightWebView::recreate`].
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if let Some(view) = self.sendable() {
            if let Ok(body) = wire::encode_set_visible_args(&wire::SetVisibleArgs { view, visible })
            {
                self.send(wire::METHOD_SETVISIBLE, body);
            }
        }
    }

    /// Ask for a whole new frame (the app discarded its picture).
    pub fn refresh(&mut self) {
        if let Some(view) = self.sendable() {
            if let Ok(body) = wire::encode_refresh_args(&wire::RefreshArgs { view }) {
                self.send(wire::METHOD_REFRESH, body);
            }
        }
    }

    /// The view gained or lost keyboard focus.
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(view) = self.sendable() {
            if let Ok(body) = wire::encode_focus_args(&wire::FocusArgs { view, focused }) {
                self.send(wire::METHOD_FOCUS, body);
            }
        }
    }

    /// A mouse event at `(x, y)` in view pixels; `button` is 0 left, 1 middle, 2 right.
    pub fn mouse(&mut self, kind: MouseKind, button: i32, x: i32, y: i32, mods: u32) {
        if let Some(view) = self.sendable() {
            let args = wire::MouseArgs {
                view,
                kind: kind.wire(),
                button,
                x,
                y,
                mods,
            };
            if let Ok(body) = wire::encode_mouse_args(&args) {
                self.send(wire::METHOD_MOUSE, body);
            }
        }
    }

    /// A wheel turn of `ticks_x` / `ticks_y` notches at `(x, y)`.
    pub fn wheel(&mut self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        if let Some(view) = self.sendable() {
            let args = wire::WheelArgs {
                view,
                x,
                y,
                ticks_x,
                ticks_y,
                mods,
            };
            if let Ok(body) = wire::encode_wheel_args(&args) {
                self.send(wire::METHOD_WHEEL, body);
            }
        }
    }

    /// A key edge: `keysym` is the X11 keysym, `text` the character it types (empty for none).
    pub fn key(&mut self, press: bool, keysym: u32, text: &str, mods: u32) {
        if let Some(view) = self.sendable() {
            let args = wire::KeyArgs {
                view,
                press,
                keysym,
                text: text.to_owned(),
                mods,
            };
            if let Ok(body) = wire::encode_key_args(&args) {
                self.send(wire::METHOD_KEY, body);
            }
        }
    }

    /// The view's id when there is a live one to send to.
    fn sendable(&self) -> Option<u64> {
        self.live_view().ok()
    }
}
