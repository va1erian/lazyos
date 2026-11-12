//! The `os.lazy.input.v1` client: an app's keyboard session with `inputd`.
//!
//! The static-musl counterpart of the native `user::messenger::input` module,
//! built on the generated client (`idl/input.midl`). A window opens a session for its surface right after
//! `CreateSurface`, handing `inputd` its own event endpoint; from then on keys
//! arrive there — `KeyEvent` for the physical key and its meaning,
//! `TextInput` for composed characters — instead of as `KeyDown`/`KeyUp` from
//! the compositor. Without `inputd` (an image built without the service) the
//! open fails and the window simply stays on the compositor's legacy keys.

use libmessenger::Parcel;
use messenger_generated::os_lazy_input_v1 as wire;

use crate::platform::messenger::{client, Observed};
use crate::sys::{errno, msg_create_pair};
use std::rc::Rc;

mod grab;
pub use grab::{KeyStatePage, Snapshot};

/// Interface id every input parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Modifier bits in `KeyEvent.mods` (the wire values documented in
/// `idl/input.midl`).
pub mod mods {
    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const SUPER: u32 = 1 << 3;
}

/// Keysyms (`inputmap::keysym`, X11 values) the backend maps to `xui` keys.
pub mod keysym {
    pub const SHIFT_L: u32 = 0xFFE1;
    pub const SHIFT_R: u32 = 0xFFE2;
    pub const CONTROL_L: u32 = 0xFFE3;
    pub const CONTROL_R: u32 = 0xFFE4;
    pub const ALT_L: u32 = 0xFFE9;
    pub const ALT_R: u32 = 0xFFEA;
    pub const SUPER_L: u32 = 0xFFEB;
    pub const SUPER_R: u32 = 0xFFEC;
    pub const BACKSPACE: u32 = 0xFF08;
    pub const TAB: u32 = 0xFF09;
    pub const ENTER: u32 = 0xFF0D;
    pub const ESCAPE: u32 = 0xFF1B;
    pub const HOME: u32 = 0xFF50;
    pub const LEFT: u32 = 0xFF51;
    pub const UP: u32 = 0xFF52;
    pub const RIGHT: u32 = 0xFF53;
    pub const DOWN: u32 = 0xFF54;
    pub const PAGE_UP: u32 = 0xFF55;
    pub const PAGE_DOWN: u32 = 0xFF56;
    pub const END: u32 = 0xFF57;
    pub const INSERT: u32 = 0xFF63;
    pub const KP_ENTER: u32 = 0xFF8D;
    pub const F1: u32 = 0xFFBE;
    pub const F12: u32 = 0xFFC9;
    pub const DELETE: u32 = 0xFFFF;
}

/// What happened to a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Down,
    Up,
    /// Auto-repeat, never confused with a fresh press.
    Repeat,
}

/// One event `inputd` sent on the session endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A key changed state: the physical `code` (HID usage), its `sym` under
    /// the active layout and the modifier bits after the change.
    Key {
        code: u32,
        sym: u32,
        mods: u32,
        state: KeyState,
    },
    /// Composed text for a character-producing press.
    Text(String),
    /// Keyboard focus arrived; `down` lists the keys already held.
    Enter(Vec<u32>),
    /// Keyboard focus left: every key must be treated as released.
    Leave,
    /// The layout changed.
    Layout(String),
    /// The session's keyboard grab started (`active`) or ended; `reason`
    /// is the wire's `GrantReason` (`wire::GRANT_REASON_*`).
    Grant { active: bool, reason: u32 },
}

/// Decode one session event; `None` for anything unknown or malformed. A key
/// event for usage `0` (an empty body decodes to zeros) is not a key.
pub fn decode_event(parcel: &Parcel) -> Option<Event> {
    let body = &parcel.body;
    Some(match parcel.header.method {
        wire::METHOD_KEYEVENT => {
            let args = wire::decode_key_event_args(body).ok()?;
            let state = match args.state {
                wire::KEY_STATE_DOWN => KeyState::Down,
                wire::KEY_STATE_UP => KeyState::Up,
                wire::KEY_STATE_REPEAT => KeyState::Repeat,
                _ => return None,
            };
            if args.code == 0 {
                return None;
            }
            Event::Key {
                code: args.code,
                sym: args.sym,
                mods: args.mods,
                state,
            }
        }
        wire::METHOD_TEXTINPUT => Event::Text(wire::decode_text_input_args(body).ok()?.utf8),
        wire::METHOD_KEYBOARDENTER => {
            Event::Enter(wire::decode_keyboard_enter_args(body).ok()?.down)
        }
        wire::METHOD_KEYBOARDLEAVE => Event::Leave,
        wire::METHOD_LAYOUTCHANGED => {
            Event::Layout(wire::decode_layout_changed_args(body).ok()?.layout)
        }
        wire::METHOD_GRANTCHANGED => {
            let args = wire::decode_grant_changed_args(body).ok()?;
            Event::Grant {
                active: args.active,
                reason: args.reason,
            }
        }
        _ => return None,
    })
}

/// An open input session: its id and this task's end of the event channel.
#[derive(Clone, Copy, Debug)]
pub struct Session {
    pub session: u64,
    /// The event endpoint handle in this task's table.
    pub events: u64,
}

impl Session {
    /// Open a session for `surface` (one this task created). `Err` carries a
    /// negative errno; the usual one is `-ENOENT` when `inputd` is not
    /// running, which callers treat as "use the compositor's legacy keys".
    pub fn open(surface: u64) -> Result<Session, i64> {
        let service = service()?;
        Session::open_on(&service, surface)
    }

    fn open_on(service: &Inputd, surface: u64) -> Result<Session, i64> {
        let (events, peer) = msg_create_pair()?;
        match service.open(Some(surface), peer) {
            // Session ids start at 1; zero is a missing field.
            Ok(session) if session != 0 => Ok(Session { session, events }),
            other => {
                // The peer may or may not have moved; closing a stale handle
                // only fails harmlessly.
                let _ = crate::display::close(peer);
                let _ = crate::display::close(events);
                Err(other.err().map_or(-errno::EINVAL, i64::from))
            }
        }
    }

    /// End the session and close the event channel. Best effort: the compositor
    /// destroying the surface ends it too.
    pub fn close(&self) {
        if let Ok(service) = service() {
            let _ = service.close(self.session);
        }
        let _ = crate::display::close(self.events);
    }
}

/// The generated client of `inputd`, which waits `inputd` for what the `.midl`
/// states for `Open`/`Close` and the rest: it answers from its receive loop,
/// and a hung service must not freeze window creation.
type Inputd = wire::Client<Observed>;

/// This thread's client of `inputd`, without waiting for it to register: an
/// image without it falls back to the compositor's legacy keys at once.
fn service() -> Result<Rc<Inputd>, i64> {
    Ok(client(wire::Client::try_connect)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmessenger::{Header, VERSION};

    fn parcel(method: u32, body: Vec<u8>) -> Parcel {
        Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: INTERFACE,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            ..Parcel::default()
        }
    }

    #[test]
    fn key_events_decode_with_their_state() {
        for (wire_state, state) in [
            (wire::KEY_STATE_DOWN, KeyState::Down),
            (wire::KEY_STATE_UP, KeyState::Up),
            (wire::KEY_STATE_REPEAT, KeyState::Repeat),
        ] {
            let body = wire::encode_key_event_args(&wire::KeyEventArgs {
                code: 0x1A,
                sym: 'w' as u32,
                mods: mods::CTRL,
                state: wire_state,
                ts_ns: 5,
                seq: 6,
            })
            .unwrap();
            assert_eq!(
                decode_event(&parcel(wire::METHOD_KEYEVENT, body)),
                Some(Event::Key {
                    code: 0x1A,
                    sym: 'w' as u32,
                    mods: mods::CTRL,
                    state
                })
            );
        }
    }

    #[test]
    fn a_key_event_for_usage_zero_or_an_unknown_state_is_dropped() {
        assert_eq!(
            decode_event(&parcel(wire::METHOD_KEYEVENT, Vec::new())),
            None
        );
        let body = wire::encode_key_event_args(&wire::KeyEventArgs {
            code: 4,
            sym: 97,
            mods: 0,
            state: 9,
            ts_ns: 0,
            seq: 0,
        })
        .unwrap();
        assert_eq!(decode_event(&parcel(wire::METHOD_KEYEVENT, body)), None);
    }

    #[test]
    fn text_enter_leave_and_layout_decode() {
        let text = wire::encode_text_input_args(&wire::TextInputArgs {
            utf8: "é".to_string(),
        })
        .unwrap();
        assert_eq!(
            decode_event(&parcel(wire::METHOD_TEXTINPUT, text)),
            Some(Event::Text("é".to_string()))
        );
        let enter = wire::encode_keyboard_enter_args(&wire::KeyboardEnterArgs {
            down: vec![0xE1, 4],
        })
        .unwrap();
        assert_eq!(
            decode_event(&parcel(wire::METHOD_KEYBOARDENTER, enter)),
            Some(Event::Enter(vec![0xE1, 4]))
        );
        assert_eq!(
            decode_event(&parcel(wire::METHOD_KEYBOARDLEAVE, Vec::new())),
            Some(Event::Leave)
        );
        let layout = wire::encode_layout_changed_args(&wire::LayoutChangedArgs {
            layout: "fr".to_string(),
        })
        .unwrap();
        assert_eq!(
            decode_event(&parcel(wire::METHOD_LAYOUTCHANGED, layout)),
            Some(Event::Layout("fr".to_string()))
        );
        assert_eq!(decode_event(&parcel(999, Vec::new())), None);
    }
}
