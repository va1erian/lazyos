//! A full view's oneway messages: visibility, focus, refresh and input, as on a light view
//! (`crate::input`). None waits for webd or returns a result.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::FullWebView;
use crate::input::MouseKind;
use crate::transport::ScopedTransport;

fn kind_wire(kind: MouseKind) -> u32 {
    match kind {
        MouseKind::Move => wire::MOUSE_KIND_MOVE,
        MouseKind::Press => wire::MOUSE_KIND_PRESS,
        MouseKind::Release => wire::MOUSE_KIND_RELEASE,
    }
}

impl<T: ScopedTransport> FullWebView<T> {
    /// A view that is not visible gets no paints. Remembered across [`FullWebView::create`].
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if let Ok(view) = self.live_view() {
            if let Ok(body) = wire::encode_set_visible_args(&wire::SetVisibleArgs { view, visible })
            {
                self.send_owed(wire::METHOD_SETVISIBLE, body, true);
            }
        }
    }

    /// Ask for a whole new frame.
    pub fn refresh(&mut self) {
        if let Ok(view) = self.live_view() {
            if let Ok(body) = wire::encode_refresh_args(&wire::RefreshArgs { view }) {
                self.send(wire::METHOD_REFRESH, body);
            }
        }
    }

    /// The view gained or lost keyboard focus.
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
        if let Ok(view) = self.live_view() {
            if let Ok(body) = wire::encode_focus_args(&wire::FocusArgs { view, focused }) {
                self.send_owed(wire::METHOD_FOCUS, body, true);
            }
        }
    }

    /// A mouse event at `(x, y)` in view pixels; `button` is 0 left, 1 middle, 2 right. A press or
    /// a release is a user gesture to webd (a page may then hand an address over).
    pub fn mouse(&mut self, kind: MouseKind, button: i32, x: i32, y: i32, mods: u32) {
        if let Ok(view) = self.live_view() {
            let args = wire::MouseArgs {
                view,
                kind: kind_wire(kind),
                button,
                x,
                y,
                mods,
            };
            if let Ok(body) = wire::encode_mouse_args(&args) {
                match kind {
                    MouseKind::Release => self.send_owed(wire::METHOD_MOUSE, body, false),
                    _ => self.send(wire::METHOD_MOUSE, body),
                }
            }
        }
    }

    /// A wheel turn of `ticks_x` / `ticks_y` notches at `(x, y)` (a negative `ticks_y` scrolls
    /// down). A non-zero `seq` (rising) is answered with [`FullEvent::WheelDone`](super::FullEvent)
    /// once the page has answered the turn.
    pub fn wheel(&mut self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32, seq: u64) {
        if let Ok(view) = self.live_view() {
            let args = wire::WheelArgs {
                view,
                x,
                y,
                ticks_x,
                ticks_y,
                mods,
                seq,
            };
            if let Ok(body) = wire::encode_wheel_args(&args) {
                self.send(wire::METHOD_WHEEL, body);
            }
        }
    }

    /// A key edge: `keysym` is the X11 keysym, `text` the character it types (empty for none).
    pub fn key(&mut self, press: bool, keysym: u32, text: &str, mods: u32) {
        if let Ok(view) = self.live_view() {
            let args = wire::KeyArgs {
                view,
                press,
                keysym,
                text: text.to_owned(),
                mods,
            };
            if let Ok(body) = wire::encode_key_args(&args) {
                if press {
                    self.send(wire::METHOD_KEY, body);
                } else {
                    self.send_owed(wire::METHOD_KEY, body, false);
                }
            }
        }
    }
}
