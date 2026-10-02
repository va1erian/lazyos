//! The service half of `inputd`: sessions, focus routing and event delivery.
//!
//! The pure decisions (who may open what, who has focus) live in
//! `inputmap::Router`; this module owns the endpoints those decisions refer to
//! and turns them into Messenger traffic. Key content goes to exactly one
//! place: the endpoint of the focused session. A session whose endpoint fills
//! up gets the rest from a bounded backlog as it drains (`delivery.rs`).

use alloc::format;
use alloc::vec::Vec;

use inputmap::router::Error as RouteError;
use inputmap::{
    Engine, KeyOut, KeyState, Layout, Output, Router, REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS,
};
use user::messenger::input::{self as api, shell_wire, wire};
use user::messenger::{errno, services, Endpoint, Error, Message, Parcel, Result};
use user::sys;

use super::delivery::{Delivery, Reach};
use super::pointer::Cursor;

/// Most hotkey chords the compositor may register.
const MAX_HOTKEYS: usize = 64;

/// PIT ticks are 10 ms.
const TICK_MS: u32 = 10;

const ENOSYS: i64 = 38;

/// What the generated encoders return.
type Encoded = core::result::Result<Vec<u8>, libmessenger::Error>;

/// The attached compositor.
struct Shell {
    /// Its kernel-stamped task slot: every shell call must come from it.
    sender: u64,
    /// Where shell events go.
    events: Endpoint,
    /// Chords it registered, removed again when it is replaced or gone.
    hotkeys: Vec<u64>,
}

pub(super) struct Hub {
    pub(super) engine: Engine,
    router: Router,
    /// The sessions' endpoints and their backlogs.
    delivery: Delivery,
    shell: Option<Shell>,
    /// The cursor every pointing device moves (`pointer.rs`).
    pub(super) pointer: Cursor,
}

impl Hub {
    pub(super) fn new(layout: Layout) -> Hub {
        Hub {
            engine: Engine::new(layout),
            router: Router::new(),
            delivery: Delivery::default(),
            shell: None,
            pointer: Cursor::new(),
        }
    }

    // ---- requests ---------------------------------------------------------

    /// Route one inbound call. `Ok(parcel)` is the reply; `Err` becomes the
    /// structured error reply the caller sees.
    pub(super) fn handle(&mut self, message: &Message) -> Result<Parcel> {
        let interface = message.interface_id();
        let method = message.method();
        // A request carries exactly what `input.midl` declares for it (`Open`
        // and `Attach`: one channel); anything else is closed and refused
        // before dispatch, so no path can leave it in this task's table.
        let declared = if interface == api::SHELL_INTERFACE {
            shell_wire::request_transfers(method)
        } else {
            wire::request_transfers(method)
        };
        if !message.carries(declared) {
            release_transfers(message);
            return Err(Error::Errno(-errno::EINVAL));
        }
        let body = if interface == api::INTERFACE {
            self.client_call(message)?
        } else if interface == api::SHELL_INTERFACE {
            self.shell_call(message)?
        } else {
            release_transfers(message);
            return Err(Error::Errno(-errno::EINVAL));
        };
        Ok(api::request(interface, method, body, Vec::new()))
    }

    /// The error reply for a refused request.
    pub(super) fn error_reply(message: &Message, error: Error) -> Parcel {
        services::error_reply(message.interface_id(), message.method(), error)
    }

    fn client_call(&mut self, message: &Message) -> Result<Vec<u8>> {
        let body = &message.parcel.body;
        match message.method() {
            wire::METHOD_OPEN => self.open(message),
            wire::METHOD_CLOSE => {
                release_transfers(message);
                let args = wire::decode_close_args(body).map_err(Error::Parcel)?;
                let surface = self
                    .router
                    .close(args.session, message.sender)
                    .map_err(route_error)?;
                self.forget_endpoint(args.session);
                self.announce_closed(surface);
                Ok(Vec::new())
            }
            wire::METHOD_GETSTATE => {
                release_transfers(message);
                wire::encode_get_state_reply(&wire::GetStateReply {
                    layout: self.engine.layout().name().into(),
                    mods: self.engine.mods(),
                    repeat_delay_ms: REPEAT_DELAY_TICKS as u32 * TICK_MS,
                    repeat_interval_ms: REPEAT_INTERVAL_TICKS as u32 * TICK_MS,
                })
                .map_err(Error::Parcel)
            }
            _ => {
                release_transfers(message);
                Err(Error::Errno(-errno::EINVAL))
            }
        }
    }

    /// `Open`: bind a session to a surface the sender owns and adopt the event
    /// endpoint it transferred.
    fn open(&mut self, message: &Message) -> Result<Vec<u8>> {
        let result = self.open_inner(message);
        if result.is_err() {
            release_transfers(message);
        }
        result
    }

    fn open_inner(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args = wire::decode_open_args(&message.parcel.body).map_err(Error::Parcel)?;
        // A session without a surface is reserved for the login console.
        let surface = args.surface.ok_or(Error::Errno(-errno::EINVAL))?;
        if !message.carries(wire::OPEN_TRANSFERS) {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let opened = self
            .router
            .open(message.sender, surface)
            .map_err(route_error)?;
        if let Some(old) = opened.replaced {
            self.forget_endpoint(old);
        }
        self.delivery
            .insert(opened.session, Endpoint::from_raw(message.first_handle));
        // Sessions are rare (one per window), so each is worth a boot-log line.
        sys::write_str(&format!(
            "INPUTD:SESSION:OPEN session={} surface={surface} owner={}
",
            opened.session, message.sender
        ));
        if opened.first_for_surface {
            self.shell_event(
                shell_wire::METHOD_SESSIONOPENED,
                shell_wire::encode_session_opened_args(&shell_wire::SessionOpenedArgs { surface }),
            );
        }
        if opened.focused {
            self.enter(opened.session);
        }
        wire::encode_open_reply(&wire::OpenReply {
            session: opened.session,
        })
        .map_err(Error::Parcel)
    }

    fn shell_call(&mut self, message: &Message) -> Result<Vec<u8>> {
        let method = message.method();
        if method == shell_wire::METHOD_ATTACH {
            return self.attach(message);
        }
        release_transfers(message);
        // Everything else is the attached compositor's alone.
        if self.shell.as_ref().map(|shell| shell.sender) != Some(message.sender) {
            return Err(Error::Errno(-errno::EACCES));
        }
        let body = &message.parcel.body;
        match method {
            shell_wire::METHOD_SETFOCUS => {
                let args = shell_wire::decode_set_focus_args(body).map_err(Error::Parcel)?;
                self.set_focus(args.surface);
                Ok(Vec::new())
            }
            shell_wire::METHOD_REGISTERSURFACE => {
                let args = shell_wire::decode_register_surface_args(body).map_err(Error::Parcel)?;
                self.router
                    .register_surface(args.surface, args.owner)
                    .map_err(route_error)?;
                Ok(Vec::new())
            }
            shell_wire::METHOD_UNREGISTERSURFACE => {
                let args =
                    shell_wire::decode_unregister_surface_args(body).map_err(Error::Parcel)?;
                if let Some(session) = self.router.unregister_surface(args.surface) {
                    self.forget_endpoint(session);
                }
                Ok(Vec::new())
            }
            shell_wire::METHOD_REGISTERHOTKEY => {
                let args = shell_wire::decode_register_hotkey_args(body).map_err(Error::Parcel)?;
                let code = u16::try_from(args.code).map_err(|_| Error::Errno(-errno::EINVAL))?;
                if self.engine.hotkey_count() >= MAX_HOTKEYS {
                    return Err(Error::Errno(-errno::ENOMEM));
                }
                let id = self.engine.add_hotkey(code, args.mods);
                if let Some(shell) = self.shell.as_mut() {
                    shell.hotkeys.push(id);
                }
                shell_wire::encode_register_hotkey_reply(&shell_wire::RegisterHotkeyReply { id })
                    .map_err(Error::Parcel)
            }
            shell_wire::METHOD_UNREGISTERHOTKEY => {
                let args =
                    shell_wire::decode_unregister_hotkey_args(body).map_err(Error::Parcel)?;
                if self.engine.remove_hotkey(args.id) {
                    if let Some(shell) = self.shell.as_mut() {
                        shell.hotkeys.retain(|id| *id != args.id);
                    }
                    Ok(Vec::new())
                } else {
                    Err(Error::Errno(-errno::ENOENT))
                }
            }
            // Keyboard grabs are a later phase; the method is reserved.
            shell_wire::METHOD_APPROVEGRANT => Err(Error::Errno(-ENOSYS)),
            shell_wire::METHOD_SETBOUNDS | shell_wire::METHOD_GETPOINTER => {
                self.pointer_call(method, body)
            }
            _ => Err(Error::Errno(-errno::EINVAL)),
        }
    }

    /// `Attach`: only the display grant's holder (the compositor) may become
    /// the shell client.
    fn attach(&mut self, message: &Message) -> Result<Vec<u8>> {
        if !message.carries(shell_wire::ATTACH_TRANSFERS) {
            return Err(Error::Errno(-errno::EINVAL));
        }
        if !is_compositor(message.sender) {
            release_transfers(message);
            return Err(Error::Errno(-errno::EACCES));
        }
        self.drop_shell();
        self.shell = Some(Shell {
            sender: message.sender,
            events: Endpoint::from_raw(message.first_handle),
            hotkeys: Vec::new(),
        });
        // A re-attaching compositor has forgotten which surfaces take keys
        // through a session; tell it, or it would forward legacy keys too.
        let surfaces: Vec<u64> = self
            .router
            .sessions()
            .filter_map(|session| self.router.session(session))
            .map(|session| session.surface)
            .collect();
        for surface in surfaces {
            self.shell_event(
                shell_wire::METHOD_SESSIONOPENED,
                shell_wire::encode_session_opened_args(&shell_wire::SessionOpenedArgs { surface }),
            );
        }
        Ok(Vec::new())
    }

    // ---- focus ------------------------------------------------------------

    /// Move keyboard focus. Repeat is cancelled so a held key cannot leak from
    /// one window into the next.
    fn set_focus(&mut self, surface: Option<u64>) {
        let change = self.router.set_focus(surface);
        self.engine.cancel_repeat();
        if let Some(session) = change.left {
            self.send(session, wire::METHOD_KEYBOARDLEAVE, Ok(Vec::new()));
        }
        if let Some(session) = change.entered {
            self.enter(session);
        }
    }

    /// Tell `session` it has the keyboard, seeding it with the held keys.
    fn enter(&mut self, session: u64) {
        let down = self.engine.held().into_iter().map(u32::from).collect();
        let body = wire::encode_keyboard_enter_args(&wire::KeyboardEnterArgs { down });
        self.send(session, wire::METHOD_KEYBOARDENTER, body);
    }

    // ---- delivery ---------------------------------------------------------

    /// Deliver the engine's outputs: key content to the focused session only,
    /// hotkey matches to the compositor.
    pub(super) fn deliver(&mut self, outputs: &[Output]) {
        for output in outputs {
            match output {
                Output::Hotkey(id) => self.shell_event(
                    shell_wire::METHOD_HOTKEYFIRED,
                    shell_wire::encode_hotkey_fired_args(&shell_wire::HotkeyFiredArgs { id: *id }),
                ),
                Output::Key(key) => {
                    if let Some(session) = self.router.focused_session() {
                        self.send(session, wire::METHOD_KEYEVENT, encode_key(key));
                    }
                }
                Output::Text(text) => {
                    if let Some(session) = self.router.focused_session() {
                        let body = wire::encode_text_input_args(&wire::TextInputArgs {
                            utf8: text.clone(),
                        });
                        self.send(session, wire::METHOD_TEXTINPUT, body);
                    }
                }
            }
        }
    }

    /// The layout changed: adopt it and tell every session (the layout name is
    /// not secret).
    pub(super) fn set_layout(&mut self, layout: Layout) {
        self.engine.set_layout(layout);
        let sessions: Vec<u64> = self.router.sessions().collect();
        for session in sessions {
            let body = wire::encode_layout_changed_args(&wire::LayoutChangedArgs {
                layout: layout.name().into(),
            });
            self.send(session, wire::METHOD_LAYOUTCHANGED, body);
        }
    }

    /// Send an event to `session` (behind any backlog it has), dropping the
    /// session when its endpoint is gone.
    fn send(&mut self, session: u64, method: u32, body: Encoded) {
        let Ok(body) = body else { return };
        let engine = &self.engine;
        let enter = || keyboard_enter(engine);
        if self.delivery.send(session, method, body, &enter) == Reach::Gone {
            self.drop_session(session);
        }
    }

    /// Hand every backlog what its client has room for now (once per pass of
    /// the service loop).
    pub(super) fn flush(&mut self) {
        let engine = &self.engine;
        let enter = || keyboard_enter(engine);
        for session in self.delivery.flush_all(&enter) {
            self.drop_session(session);
        }
    }

    pub(super) fn shell_event(&mut self, method: u32, body: Encoded) {
        let (Some(shell), Ok(body)) = (&self.shell, body) else {
            return;
        };
        let parcel = api::event(api::SHELL_INTERFACE, method, body);
        if let Err(Error::Errno(code)) = shell.events.send(&parcel) {
            if code == -errno::EPIPE {
                // The compositor is gone: nobody is focused until it returns.
                self.drop_shell();
                self.set_focus(None);
            }
        }
    }

    /// Forget the attached compositor: close its event endpoint and remove
    /// the chords it registered (a re-attach registers them again).
    fn drop_shell(&mut self) {
        self.pointer.subscribed = false;
        if let Some(old) = self.shell.take() {
            let _ = old.events.close();
            for id in old.hotkeys {
                self.engine.remove_hotkey(id);
            }
        }
    }

    // ---- teardown ---------------------------------------------------------

    fn forget_endpoint(&mut self, session: u64) {
        self.delivery.forget(session);
    }

    /// A session's endpoint died: drop it and tell the compositor.
    fn drop_session(&mut self, session: u64) {
        if let Some(removed) = self.router.remove(session) {
            self.forget_endpoint(session);
            self.announce_closed(removed.surface);
        }
    }

    /// Tell the compositor `surface` has no session (legacy delivery resumes).
    fn announce_closed(&mut self, surface: u64) {
        if !self.router.has_session(surface) {
            sys::write_str(&format!(
                "INPUTD:SESSION:CLOSE surface={surface}
"
            ));
            self.shell_event(
                shell_wire::METHOD_SESSIONCLOSED,
                shell_wire::encode_session_closed_args(&shell_wire::SessionClosedArgs { surface }),
            );
        }
    }
}

/// The `KeyboardEnter` body for the keys held right now.
fn keyboard_enter(engine: &Engine) -> Option<Vec<u8>> {
    let down = engine.held().into_iter().map(u32::from).collect();
    wire::encode_keyboard_enter_args(&wire::KeyboardEnterArgs { down }).ok()
}

fn encode_key(key: &KeyOut) -> Encoded {
    wire::encode_key_event_args(&wire::KeyEventArgs {
        code: u32::from(key.code),
        sym: key.sym,
        mods: key.mods,
        state: match key.state {
            KeyState::Down => wire::KEY_STATE_DOWN,
            KeyState::Up => wire::KEY_STATE_UP,
            KeyState::Repeat => wire::KEY_STATE_REPEAT,
        },
        ts_ns: key.ts_ns,
        seq: key.seq,
    })
}

/// Map a router refusal onto its errno.
fn route_error(error: RouteError) -> Error {
    Error::Errno(-match error {
        RouteError::NoSurface | RouteError::NoSession => errno::ENOENT,
        RouteError::NotOwner => errno::EACCES,
        RouteError::Full => errno::ENOMEM,
    })
}

/// Close whatever a refused (or non-adopting) request transferred, endpoint
/// and buffer alike, so neither leaks into this task's handle table.
fn release_transfers(message: &Message) {
    if message.handles != 0 {
        let _ = Endpoint::from_raw(message.first_handle).close();
    }
    if message.buffers != 0 {
        let _ = sys::display_close_buffer(message.first_buffer);
    }
}

/// Whether `sender` holds the display grant: the compositor. The kernel says
/// who that is, so no client can pose as it by registering a name.
fn is_compositor(sender: u64) -> bool {
    sys::input_display_owner() == Ok(sender)
}
