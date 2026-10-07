//! What the shell sends on an item's event channel
//! (`os.lazy.shell.tray.events.v1`), decoded.

use crate::events_wire as wire;

/// A rectangle in screen pixels: the icon a click landed on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl From<wire::Rect> for Rect {
    fn from(rect: wire::Rect) -> Rect {
        Rect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        }
    }
}

/// One event about the app's item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Primary click; `popup` is a one-shot flyout token (0: none).
    Activate { anchor: Rect, popup: u64 },
    /// Secondary click on an item without a menu.
    SecondaryActivate { anchor: Rect },
    /// Menu row `id` was picked; `checked` is a check or radio row's new state.
    MenuItem { id: u32, checked: bool },
    /// The wheel rolled `delta` notches over the icon (positive: up).
    Scroll { delta: i32 },
    /// The shell's liveness probe: nothing to do.
    Ping,
}

/// Decode one message from the event channel. `None` for another interface,
/// an unknown method or a malformed body: the sender is the shell, but a
/// client never trusts a body enough to panic on it.
pub fn decode_event(interface: u64, method: u32, body: &[u8]) -> Option<Event> {
    if interface != wire::INTERFACE_ID {
        return None;
    }
    let event = match method {
        wire::METHOD_ACTIVATE => {
            let args = wire::decode_activate_args(body).ok()?;
            Event::Activate {
                anchor: args.anchor.into(),
                popup: args.popup,
            }
        }
        wire::METHOD_SECONDARYACTIVATE => Event::SecondaryActivate {
            anchor: wire::decode_secondary_activate_args(body)
                .ok()?
                .anchor
                .into(),
        },
        wire::METHOD_MENUITEM => {
            let args = wire::decode_menu_item_args(body).ok()?;
            Event::MenuItem {
                id: args.id,
                checked: args.checked,
            }
        }
        wire::METHOD_SCROLL => Event::Scroll {
            delta: wire::decode_scroll_args(body).ok()?.delta,
        },
        wire::METHOD_PING => Event::Ping,
        _ => return None,
    };
    Some(event)
}
