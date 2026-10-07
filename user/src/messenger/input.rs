//! The input service protocol (`docs/input-plan.md`): the generated
//! `os.lazy.input.v1` and `os.lazy.input.shell.v1` stubs plus the compositor's
//! side of the shell interface.
//!
//! `inputd` serves both interfaces on one endpoint, registered under both
//! names. Ordinary clients (the static-musl xui apps carry their own copy of
//! the client half) `Open` a session for a surface they own; the compositor
//! ([`ShellLink`]) declares surfaces and focus and receives [`ShellEvent`]s.

use alloc::vec::Vec;

use libmessenger::{Header, Parcel, VERSION};

use super::services::error_field;
use super::{create_pair, registry, Endpoint, Error, Message, Result, DEFAULT_BUFFER};
use crate::sys;

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

/// Ticks a compositor call to `inputd` may wait. `inputd` answers from its
/// receive loop, so a healthy service replies within a tick; a hung one must
/// not freeze the compositor, and a missed registration only means that
/// client falls back to legacy key delivery.
const CALL_TICKS: u64 = 20;

/// A request parcel for `interface_id`.
pub fn request(interface_id: u64, method: u32, body: Vec<u8>, handles: Vec<u64>) -> Parcel {
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
        handles,
        ..Parcel::default()
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
    input: Endpoint,
    /// This task's end of the shell event channel.
    events: Endpoint,
    buffer: Vec<u8>,
}

impl ShellLink {
    /// Resolve `inputd`, create the event channel and `Attach`. The peer end
    /// moves to `inputd` inside the call.
    pub fn connect() -> Result<ShellLink> {
        let input = registry::resolve(SHELL_NAME)?;
        let (events, peer) = create_pair()?;
        let link = ShellLink {
            input,
            events,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        };
        let (handles, _) = shell_wire::encode_attach_transfers(&shell_wire::AttachTransfers {
            events: peer.handle(),
        });
        let parcel = request(
            SHELL_INTERFACE,
            shell_wire::METHOD_ATTACH,
            Vec::new(),
            handles,
        );
        if let Err(error) = link.call(&parcel) {
            // The peer may or may not have moved; closing a stale handle only
            // fails harmlessly.
            let _ = peer.close();
            link.close();
            return Err(error);
        }
        Ok(link)
    }

    /// Release both endpoints (a failed connect, or shutting down). The
    /// `inputd` handle came from name resolution, which hands every client
    /// the same side of `inputd`'s channel: it is released, never closed,
    /// or `inputd`'s own endpoint would report a closed peer forever.
    pub fn close(&self) {
        let _ = self.input.release();
        let _ = self.events.close();
    }

    fn call(&self, parcel: &Parcel) -> Result<Parcel> {
        let mut buffer = alloc::vec![0u8; 256];
        let reply = self
            .input
            .call_with(parcel, &mut buffer, Some(sys::clock() + CALL_TICKS))?;
        match error_field(&reply)? {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(reply),
        }
    }

    fn shell_call(&self, method: u32, body: Vec<u8>) -> Result<Parcel> {
        self.call(&request(SHELL_INTERFACE, method, body, Vec::new()))
    }

    /// Send a one-way shell request: never waits on `inputd`
    /// (docs/performance-plan.md P3.6). `Err` only when it could not be
    /// queued (a full queue is retried later; a dead peer drops the link).
    fn shell_note(&self, method: u32, body: Vec<u8>) -> Result<()> {
        self.input
            .send(&request(SHELL_INTERFACE, method, body, Vec::new()))
    }

    /// One-way: every pointer event up to `seq` is handled, and the focus it
    /// led to was noted before this (`NoteInputDone`; keys typed after a
    /// click are held by `inputd` until it arrives).
    pub fn note_input_done(&self, seq: u64) -> Result<()> {
        let body = shell_wire::encode_note_input_done_args(&shell_wire::NoteInputDoneArgs { seq })
            .map_err(Error::Parcel)?;
        self.shell_note(shell_wire::METHOD_NOTEINPUTDONE, body)
    }

    /// One-way [`ShellLink::register_surface`].
    pub fn note_surface(&self, surface: u64, owner: u64) -> Result<()> {
        let body =
            shell_wire::encode_note_surface_args(&shell_wire::NoteSurfaceArgs { surface, owner })
                .map_err(Error::Parcel)?;
        self.shell_note(shell_wire::METHOD_NOTESURFACE, body)
    }

    /// One-way [`ShellLink::unregister_surface`].
    pub fn forget_surface(&self, surface: u64) -> Result<()> {
        let body =
            shell_wire::encode_forget_surface_args(&shell_wire::ForgetSurfaceArgs { surface })
                .map_err(Error::Parcel)?;
        self.shell_note(shell_wire::METHOD_FORGETSURFACE, body)
    }

    /// One-way [`ShellLink::set_focus`].
    pub fn note_focus(&self, surface: Option<u64>) -> Result<()> {
        let body = shell_wire::encode_note_focus_args(&shell_wire::NoteFocusArgs { surface })
            .map_err(Error::Parcel)?;
        self.shell_note(shell_wire::METHOD_NOTEFOCUS, body)
    }

    /// Declare that task slot `owner` created `surface`.
    pub fn register_surface(&self, surface: u64, owner: u64) -> Result<()> {
        let body = shell_wire::encode_register_surface_args(&shell_wire::RegisterSurfaceArgs {
            surface,
            owner,
        })
        .map_err(Error::Parcel)?;
        self.shell_call(shell_wire::METHOD_REGISTERSURFACE, body)
            .map(|_| ())
    }

    /// Forget `surface` (destroyed).
    pub fn unregister_surface(&self, surface: u64) -> Result<()> {
        let body = shell_wire::encode_unregister_surface_args(&shell_wire::UnregisterSurfaceArgs {
            surface,
        })
        .map_err(Error::Parcel)?;
        self.shell_call(shell_wire::METHOD_UNREGISTERSURFACE, body)
            .map(|_| ())
    }

    /// Give keyboard focus to `surface` (`None`: nobody).
    pub fn set_focus(&self, surface: Option<u64>) -> Result<()> {
        let body = shell_wire::encode_set_focus_args(&shell_wire::SetFocusArgs { surface })
            .map_err(Error::Parcel)?;
        self.shell_call(shell_wire::METHOD_SETFOCUS, body)
            .map(|_| ())
    }

    /// Register a chord; `inputd` then consumes it (client never sees it).
    pub fn register_hotkey(&self, code: u32, mods: u32) -> Result<u64> {
        let body =
            shell_wire::encode_register_hotkey_args(&shell_wire::RegisterHotkeyArgs { code, mods })
                .map_err(Error::Parcel)?;
        let reply = self.shell_call(shell_wire::METHOD_REGISTERHOTKEY, body)?;
        Ok(shell_wire::decode_register_hotkey_reply(&reply.body)
            .map_err(Error::Parcel)?
            .id)
    }

    /// Answer a `GrantRequested`: `allow` grants the grab if the session
    /// still has focus.
    pub fn approve_grant(&self, session: u64, allow: bool) -> Result<()> {
        let body =
            shell_wire::encode_approve_grant_args(&shell_wire::ApproveGrantArgs { session, allow })
                .map_err(Error::Parcel)?;
        self.shell_call(shell_wire::METHOD_APPROVEGRANT, body)
            .map(|_| ())
    }

    /// Clamp the cursor to a `width` x `height` screen; it also subscribes this
    /// compositor to `PointerEvent`.
    pub fn set_bounds(&self, width: u32, height: u32) -> Result<()> {
        let body = shell_wire::encode_set_bounds_args(&shell_wire::SetBoundsArgs { width, height })
            .map_err(Error::Parcel)?;
        self.shell_call(shell_wire::METHOD_SETBOUNDS, body)
            .map(|_| ())
    }

    /// The cursor position and held buttons, to seed the compositor's cursor.
    pub fn get_pointer(&self) -> Result<PointerState> {
        let reply = self.shell_call(shell_wire::METHOD_GETPOINTER, Vec::new())?;
        let seed = shell_wire::decode_get_pointer_reply(&reply.body).map_err(Error::Parcel)?;
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
