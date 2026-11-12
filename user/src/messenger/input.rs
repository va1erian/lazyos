//! The input service protocol (`docs/input-plan.md`): the generated
//! `os.lazy.input.v1` and `os.lazy.input.shell.v1` stubs plus the compositor's
//! side of the shell interface.
//!
//! `inputd` serves both interfaces on one endpoint, registered under both
//! names. Ordinary clients (the static-musl xui apps carry their own copy of
//! the client half) `Open` a session for a surface they own; the compositor
//! ([`ShellLink`]) declares surfaces and focus and receives [`ShellEvent`]s.
//! After `Attach` the compositor sends every shell call on the channel it
//! handed `inputd`, which no client shares, so a client filling the shared
//! endpoint's queue cannot delay a focus change (docs/accounts-plan.md U2).

use alloc::vec::Vec;

use libmessenger::client::{CallError, Connection, Deadline};
use libmessenger::{Header, Object, Parcel, VERSION};

use super::client::{call_error, Held, Native};
use super::{create_pair, Endpoint, Message, Result, DEFAULT_BUFFER};

type CallResult<T> = core::result::Result<T, CallError>;

mod session;
pub use session::{KeyInput, KeySession};

/// The generated `os.lazy.input.shell.v1` stubs (compositor interface).
pub use messenger_generated::os_lazy_input_shell_v1 as shell_wire;
/// The generated `os.lazy.input.v1` stubs (client interface).
pub use messenger_generated::os_lazy_input_v1 as wire;

/// Registered name of the client interface.
pub const NAME: &str = "os.lazy.input.v1";
/// Registered name of the compositor interface.
pub const SHELL_NAME: &str = "os.lazy.input.shell.v1";
/// Interface id of the client interface.
pub const INTERFACE: u64 = wire::INTERFACE_ID;
/// Interface id of the compositor interface.
pub const SHELL_INTERFACE: u64 = shell_wire::INTERFACE_ID;

/// Modifier bits carried in `KeyEvent.mods` (the same values `inputmap` uses).
pub mod mods {
    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const SUPER: u32 = 1 << 3;
    pub const ALTGR: u32 = 1 << 4;
    pub const CAPS_LOCK: u32 = 1 << 5;
    pub const NUM_LOCK: u32 = 1 << 6;
    pub const SCROLL_LOCK: u32 = 1 << 7;
}

/// How long a compositor call to `inputd` may wait. `inputd` answers from its
/// receive loop, so a healthy service replies within a tick; a hung one must
/// not freeze the compositor, and a missed registration only means that
/// client falls back to legacy key delivery.
pub const CALL_BOUND: Deadline = Deadline::ticks(20);

/// A request parcel for `interface_id`.
pub fn request(interface_id: u64, method: u32, body: Vec<u8>, objects: Vec<Object>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects,
    }
}

/// An event parcel (`oneway`) for `interface_id`.
pub fn event(interface_id: u64, method: u32, body: Vec<u8>) -> Parcel {
    request(interface_id, method, body, Vec::new())
}

/// An event `inputd` sent the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    HotkeyFired(u64),
    /// The client owning `surface` asks for a grab for `session`; answer
    /// with [`ShellLink::approve_grant`].
    GrantRequested {
        session: u64,
        kind: u32,
        surface: u64,
    },
    /// The reserved escape chord was pressed (any grab is already reverted).
    EscapeChord,
    /// `surface` holds the keyboard grab now (`None`: nobody does).
    GrabChanged(Option<u64>),
    /// `surface` now takes keys through an input session.
    SessionOpened(u64),
    /// `surface`'s last input session ended.
    SessionClosed(u64),
    /// The pointer changed (`docs/usb-hid-plan.md`): screen-absolute position,
    /// the held-button mask (`1 << (usage - 1)`), and wheel notches since the
    /// previous event (positive is up / right). Position and wheel apply
    /// before the button change.
    Pointer(PointerState),
    /// A key went down on some keyboard (after `inputd` handled it).
    KeyActivity,
}

/// One `PointerEvent` from `inputd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerState {
    pub x: i32,
    pub y: i32,
    pub buttons: u32,
    pub wheel: i32,
    pub wheel_h: i32,
    /// The newest raw record's sequence number, echoed in
    /// [`ShellLink::note_input_done`].
    pub seq: u64,
}

/// Decode one shell event; `None` for anything unknown or malformed.
pub fn decode_shell_event(parcel: &Parcel) -> Option<ShellEvent> {
    let body = &parcel.body;
    Some(match parcel.header.method {
        shell_wire::METHOD_HOTKEYFIRED => {
            ShellEvent::HotkeyFired(shell_wire::decode_hotkey_fired_args(body).ok()?.id)
        }
        shell_wire::METHOD_GRANTREQUESTED => {
            let args = shell_wire::decode_grant_requested_args(body).ok()?;
            ShellEvent::GrantRequested {
                session: args.session,
                kind: args.kind,
                surface: args.surface,
            }
        }
        shell_wire::METHOD_GRABCHANGED => {
            ShellEvent::GrabChanged(shell_wire::decode_grab_changed_args(body).ok()?.surface)
        }
        shell_wire::METHOD_ESCAPECHORD => ShellEvent::EscapeChord,
        shell_wire::METHOD_KEYACTIVITY => ShellEvent::KeyActivity,
        shell_wire::METHOD_SESSIONOPENED => {
            ShellEvent::SessionOpened(shell_wire::decode_session_opened_args(body).ok()?.surface)
        }
        shell_wire::METHOD_SESSIONCLOSED => {
            ShellEvent::SessionClosed(shell_wire::decode_session_closed_args(body).ok()?.surface)
        }
        shell_wire::METHOD_POINTEREVENT => {
            let args = shell_wire::decode_pointer_event_args(body).ok()?;
            ShellEvent::Pointer(PointerState {
                x: args.x,
                y: args.y,
                buttons: args.buttons,
                wheel: args.wheel,
                wheel_h: args.wheel_h,
                seq: args.seq,
            })
        }
        _ => return None,
    })
}

/// The compositor's connection to `inputd`.
pub struct ShellLink {
    /// The generated shell client, over the private channel: no client can
    /// fill its queue, so a shell call never waits behind one.
    shell: shell_wire::Client<Held>,
    /// This task's end of the shell event channel (the private channel).
    events: Endpoint,
    buffer: Vec<u8>,
}

impl ShellLink {
    /// Resolve `inputd`, create the event channel and `Attach`. The peer end
    /// moves to `inputd` inside the call.
    pub fn connect() -> CallResult<ShellLink> {
        // `Attach` is the one call on the shared endpoint: the private
        // channel only exists once `inputd` has adopted its end. Dropping
        // this client releases the handle name resolution gave (never
        // closed: it is every client's side of `inputd`'s channel).
        let shared = shell_wire::Client::try_connect(Native)?;
        let (events, peer) = create_pair().map_err(call_error)?;
        if let Err(error) = shared.attach_within(peer.handle(), CALL_BOUND) {
            // The peer may or may not have moved; closing a stale handle only
            // fails harmlessly.
            let _ = peer.close();
            let _ = events.close();
            return Err(error);
        }
        Ok(ShellLink {
            shell: shell_wire::Client::from_connection(Connection::new(
                Held::private(events),
                SHELL_NAME,
            )),
            events,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        })
    }

    /// Close the event channel (shutting down). The `inputd` handle was
    /// released when `connect` returned.
    pub fn close(&self) {
        let _ = self.events.close();
    }

    /// Open a key session for `surface` (one of the compositor's own,
    /// registered with owner 0) over the private channel, so a client
    /// flooding the shared endpoint cannot refuse it: the trusted prompt's
    /// keyboard (docs/accounts-plan.md U2).
    pub fn open_keys(&self, surface: u64) -> CallResult<KeySession> {
        KeySession::open_on(Held::private(self.events), surface)
    }

    /// One-way: every pointer event up to `seq` is handled, and the focus it
    /// led to was noted before this (`NoteInputDone`; keys typed after a
    /// click are held by `inputd` until it arrives).
    pub fn note_input_done(&self, seq: u64) -> CallResult<()> {
        self.shell.note_input_done(seq)
    }

    /// One-way: the shell's panel menu holds (or gives back) the keyboard;
    /// focus stays, key content to the focused session is held
    /// (`NoteKeysHeld`, `inputmap::hold`).
    pub fn note_keys_held(&self, held: bool) -> CallResult<()> {
        self.shell.note_keys_held(held)
    }

    /// One-way: the logged-in user's own keyboard layout, or `None` to follow
    /// the machine default (`NoteSessionLayout`, `inputmap::session_layout`).
    pub fn note_session_layout(&self, layout: Option<&str>) -> CallResult<()> {
        self.shell.note_session_layout(layout)
    }

    /// One-way [`ShellLink::register_surface`].
    pub fn note_surface(&self, surface: u64, owner: u64) -> CallResult<()> {
        self.shell.note_surface(surface, owner)
    }

    /// One-way [`ShellLink::unregister_surface`].
    pub fn forget_surface(&self, surface: u64) -> CallResult<()> {
        self.shell.forget_surface(surface)
    }

    /// One-way [`ShellLink::set_focus`].
    pub fn note_focus(&self, surface: Option<u64>) -> CallResult<()> {
        self.shell.note_focus(surface)
    }

    /// Declare that task slot `owner` created `surface`.
    pub fn register_surface(&self, surface: u64, owner: u64) -> CallResult<()> {
        self.shell
            .register_surface_within(surface, owner, CALL_BOUND)
    }

    /// Forget `surface` (destroyed).
    pub fn unregister_surface(&self, surface: u64) -> CallResult<()> {
        self.shell.unregister_surface_within(surface, CALL_BOUND)
    }

    /// Give keyboard focus to `surface` (`None`: nobody).
    pub fn set_focus(&self, surface: Option<u64>) -> CallResult<()> {
        self.shell.set_focus_within(surface, CALL_BOUND)
    }

    /// Register a chord; `inputd` then consumes it (client never sees it).
    pub fn register_hotkey(&self, code: u32, mods: u32) -> CallResult<u64> {
        self.shell.register_hotkey_within(code, mods, CALL_BOUND)
    }

    /// Answer a `GrantRequested`: `allow` grants the grab if the session
    /// still has focus.
    pub fn approve_grant(&self, session: u64, allow: bool) -> CallResult<()> {
        self.shell.approve_grant_within(session, allow, CALL_BOUND)
    }

    /// Clamp the cursor to a `width` x `height` screen; it also subscribes this
    /// compositor to `PointerEvent`.
    pub fn set_bounds(&self, width: u32, height: u32) -> CallResult<()> {
        self.shell.set_bounds_within(width, height, CALL_BOUND)
    }

    /// The cursor position and held buttons, to seed the compositor's cursor.
    pub fn get_pointer(&self) -> CallResult<PointerState> {
        let seed = self.shell.get_pointer_within(CALL_BOUND)?;
        Ok(PointerState {
            x: seed.x,
            y: seed.y,
            buttons: seed.buttons,
            ..PointerState::default()
        })
    }

    /// This task's end of the shell event channel, for a caller that parks
    /// on it together with other endpoints (`messenger::wait::wait_any`).
    pub fn events_endpoint(&self) -> Endpoint {
        self.events
    }

    /// The next queued shell event, without blocking. `Err` means the link is
    /// dead (`inputd` went away).
    ///
    /// Events this build does not know (a newer `inputd`) or cannot decode are
    /// skipped, so `Ok(None)` always means the queue is empty.
    pub fn poll_event(&mut self) -> Result<Option<ShellEvent>> {
        loop {
            let message: Option<Message> = self.events.poll_recv_with(&mut self.buffer)?;
            let Some(message) = message else {
                return Ok(None);
            };
            if let Some(event) = decode_shell_event(&message.parcel) {
                return Ok(Some(event));
            }
        }
    }
}
