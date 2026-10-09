//! The service half of `inputd`: sessions, focus routing and event delivery.
//!
//! The pure decisions (who may open what, who has focus) live in
//! `inputmap::Router`; this module owns the endpoints those decisions refer to
//! and turns them into Messenger traffic. Key content goes to exactly one
//! place: the endpoint of the focused session. A session whose endpoint fills
//! up gets the rest from a bounded backlog as it drains (`delivery.rs`).

use alloc::format;
use alloc::vec::Vec;

use inputmap::hold::KeyHold;
use inputmap::router::Error as RouteError;
use inputmap::{Barrier, Engine, Grabs, KeyOut, KeyState, Layout, LayoutChoice, Output, Router};
use user::messenger::input::{self as api, shell_wire, wire};
use user::messenger::{errno, services, Endpoint, Error, Message, Parcel, Result};
use user::sys;

use super::delivery::{Delivery, Reach};
use super::keypages::KeyPages;
use super::pointer::Cursor;

/// Most hotkey chords the compositor may register.
const MAX_HOTKEYS: usize = 64;

/// What the generated encoders return.
pub(super) type Encoded = core::result::Result<Vec<u8>, libmessenger::Error>;

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
    pub(super) router: Router,
    /// The sessions' endpoints and their backlogs.
    pub(super) delivery: Delivery,
    shell: Option<Shell>,
    /// The cursor every pointing device moves (`pointer.rs`).
    pub(super) pointer: Cursor,
    /// Who holds the keyboard grab, who asked (`grants.rs`).
    pub(super) grabs: Grabs,
    /// The key-state pages sessions attached (`keypages.rs`).
    pub(super) key_pages: KeyPages,
    /// Key content held until the compositor has handled a press
    /// (`settle.rs`).
    pub(super) barrier: Barrier,
    /// Key content held while the shell's panel menu has the keyboard
    /// (`held.rs`).
    pub(super) hold: KeyHold,
    /// The machine layout and the logged-in user's own (`layoutsel.rs`).
    pub(super) layouts: LayoutChoice,
}

impl Hub {
    pub(super) fn new(layout: Layout) -> Hub {
        Hub {
            engine: Engine::new(layout),
            router: Router::new(),
            delivery: Delivery::default(),
            shell: None,
            pointer: Cursor::new(),
            grabs: Grabs::new(),
            key_pages: KeyPages::default(),
            barrier: Barrier::new(),
            hold: KeyHold::new(),
            layouts: LayoutChoice::new(layout),
        }
    }

    // ---- requests ---------------------------------------------------------

    /// Route one inbound call. `Ok(parcel)` is the reply; `Err` becomes the
    /// structured error reply the caller sees.
    pub(super) fn handle(&mut self, message: &Message) -> Result<Parcel> {
        let interface = message.interface_id();
        let method = message.method();
        // A request's objects (`Open`'s and `Attach`'s channel,
        // `AttachKeyState`'s page) stay the message's until their decoder
        // claims them, so a refused request leaves nothing in this task's
        // table: the message closes them when it drops.
        let body = if interface == api::INTERFACE {
            self.client_call(message)?
        } else if interface == api::SHELL_INTERFACE {
            self.shell_call(message)?
        } else {
            return Err(Error::Errno(-errno::EINVAL));
        };
        Ok(api::request(interface, method, body, Vec::new()))
    }

    /// The error reply for a refused request.
    pub(super) fn error_reply(message: &Message, error: Error) -> Parcel {
        services::error_reply(message.interface_id(), message.method(), error)
    }

    fn shell_call(&mut self, message: &Message) -> Result<Vec<u8>> {
        let method = message.method();
        if method == shell_wire::METHOD_ATTACH {
            return self.attach(message);
        }
        // Everything else is the attached compositor's alone.
        if self.shell.as_ref().map(|shell| shell.sender) != Some(message.sender) {
            return Err(Error::Errno(-errno::EACCES));
        }
        let body = &message.parcel.body;
        // The one-way twins (`Note*`, `ForgetSurface`) decode the same
        // records; their reply is simply never sent.
        match method {
            shell_wire::METHOD_SETFOCUS | shell_wire::METHOD_NOTEFOCUS => {
                let args = shell_wire::decode_set_focus_args(body).map_err(Error::Parcel)?;
                self.set_focus(args.surface);
                Ok(Vec::new())
            }
            shell_wire::METHOD_NOTEKEYSHELD => self.note_keys_held(body).map(|()| Vec::new()),
            shell_wire::METHOD_NOTESESSIONLAYOUT => {
                self.note_session_layout(body).map(|()| Vec::new())
            }
            shell_wire::METHOD_NOTEINPUTDONE => {
                let args = shell_wire::decode_note_input_done_args(body).map_err(Error::Parcel)?;
                self.input_done(args.seq);
                Ok(Vec::new())
            }
            shell_wire::METHOD_REGISTERSURFACE | shell_wire::METHOD_NOTESURFACE => {
                let args = shell_wire::decode_register_surface_args(body).map_err(Error::Parcel)?;
                // Owner 0: the compositor's own surface (its trusted prompt).
                let owner = if args.owner == 0 {
                    message.sender
                } else {
                    args.owner
                };
                self.router
                    .register_surface(args.surface, owner)
                    .map_err(route_error)?;
                Ok(Vec::new())
            }
            shell_wire::METHOD_UNREGISTERSURFACE | shell_wire::METHOD_FORGETSURFACE => {
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
                // The escape chord is nobody's to register.
                let id = self
                    .engine
                    .add_hotkey(code, args.mods)
                    .ok_or(Error::Errno(-errno::EACCES))?;
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
            shell_wire::METHOD_APPROVEGRANT => self.approve_grant(body),
            shell_wire::METHOD_SETBOUNDS | shell_wire::METHOD_GETPOINTER => {
                self.pointer_call(method, body)
            }
            _ => Err(Error::Errno(-errno::EINVAL)),
        }
    }

    /// `Attach`: only the display grant's holder (the compositor) may become
    /// the shell client.
    fn attach(&mut self, message: &Message) -> Result<Vec<u8>> {
        if !is_compositor(message.sender) {
            // The message still owns the endpoint: it closes with the message.
            return Err(Error::Errno(-errno::EACCES));
        }
        let args = message.decode(shell_wire::decode_attach_args)?;
        self.drop_shell();
        self.shell = Some(Shell {
            sender: message.sender,
            events: Endpoint::from_raw(args.events),
            hotkeys: Vec::new(),
        });
        // A re-attaching compositor has forgotten which surfaces take keys
        // through a session; tell it, or it would forward legacy keys too.
        let surfaces: Vec<u64> = self
            .router
            .sessions()
            .filter_map(|session| self.router.session(session))
            .filter_map(|session| session.surface)
            .collect();
        for surface in surfaces {
            self.shell_event(
                shell_wire::METHOD_SESSIONOPENED,
                shell_wire::encode_session_opened_args(&shell_wire::SessionOpenedArgs { surface }),
            );
        }
        // Under a compositor the console session gets nothing.
        let change = self.router.set_compositor(true);
        self.apply(change);
        // The compositor forgot a grab it was told about (`drop_input_link`
        // clears it) and any request it never answered: tell it again.
        if let Some(holder) = self.grabs.holder() {
            let surface = self.router.session(holder).and_then(|s| s.surface);
            self.shell_event(
                shell_wire::METHOD_GRABCHANGED,
                shell_wire::encode_grab_changed_args(&shell_wire::GrabChangedArgs { surface }),
            );
        }
        if let Some(session) = self.grabs.pending() {
            let surface = self
                .router
                .session(session)
                .and_then(|s| s.surface)
                .unwrap_or(0);
            self.shell_event(
                shell_wire::METHOD_GRANTREQUESTED,
                shell_wire::encode_grant_requested_args(&shell_wire::GrantRequestedArgs {
                    session,
                    kind: wire::GRANT_KIND_KEYBOARD,
                    surface,
                }),
            );
        }
        Ok(Vec::new())
    }

    // ---- focus ------------------------------------------------------------

    /// Move keyboard focus. Repeat is cancelled so a held key cannot leak from
    /// one window into the next.
    fn set_focus(&mut self, surface: Option<u64>) {
        let change = self.router.set_focus(surface);
        self.apply(change);
    }

    /// Hand the keyboard over as `change` says: cancel repeat, clear the old
    /// holder's key-state page and tell it it left, tell the new one it
    /// entered, and end a grab whose holder lost focus.
    pub(super) fn apply(&mut self, change: inputmap::router::FocusChange) {
        self.engine.cancel_repeat();
        // The keys the next holder sees start clean (no owed releases).
        self.hold.reset_keys();
        if let Some(session) = change.left {
            self.key_pages.clear(&self.engine, session);
            self.send(session, wire::METHOD_KEYBOARDLEAVE, Ok(Vec::new()));
        }
        if let Some(session) = change.entered {
            self.enter(session);
        }
        self.grants_follow_focus();
        self.publish_key_pages();
    }

    /// Bring the key-state pages up to date (after each pass and each focus
    /// change).
    pub(super) fn publish_key_pages(&mut self) {
        let focused = self.router.focused_session();
        let keys = self.page_keys();
        self.key_pages.publish(&self.engine, focused, keys);
    }

    /// Tell `session` it has the keyboard, seeding it with the held keys.
    pub(super) fn enter(&mut self, session: u64) {
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
                Output::Escape => self.escape_chord(),
                Output::Key(key) => {
                    if !self.admit_key(key) {
                        continue;
                    }
                    if let Some(session) = self.router.focused_session() {
                        self.send(session, wire::METHOD_KEYEVENT, encode_key(key));
                    }
                }
                Output::Text(text) => {
                    if let Some(session) =
                        self.router.focused_session().filter(|_| self.admit_text())
                    {
                        let body = wire::encode_text_input_args(&wire::TextInputArgs {
                            utf8: text.clone(),
                        });
                        self.send(session, wire::METHOD_TEXTINPUT, body);
                    }
                }
            }
        }
    }

    /// Send an event to `session` (behind any backlog it has), dropping the
    /// session when its endpoint is gone.
    pub(super) fn send(&mut self, session: u64, method: u32, body: Encoded) {
        let Ok(body) = body else { return };
        let engine = &self.engine;
        let enter = || keyboard_enter(engine);
        if self.delivery.send(session, method, body, &enter) == Reach::Gone {
            self.drop_session(session);
        }
    }

    /// Whether a client's backlog waits for room (the loop then retries soon).
    pub(super) fn backlogged(&self) -> bool {
        self.delivery.backlogged()
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

    /// Whether a compositor is attached (it answers grant requests).
    pub(super) fn shell_attached(&self) -> bool {
        self.shell.is_some()
    }

    pub(super) fn shell_event(&mut self, method: u32, body: Encoded) {
        let (Some(shell), Ok(body)) = (&self.shell, body) else {
            return;
        };
        let parcel = api::event(api::SHELL_INTERFACE, method, body);
        if let Err(Error::Errno(code)) = shell.events.send(&parcel) {
            if code == -errno::EPIPE {
                self.shell_lost();
            }
        }
    }

    /// The compositor is gone: no window is focused until it returns, and
    /// the console session takes the keys again.
    pub(super) fn shell_lost(&mut self) {
        self.drop_shell();
        let change = self.router.set_compositor(false);
        self.apply(change);
        self.release_keys();
    }

    /// `inputd`'s end of the attached compositor's channel: shell events go
    /// out on it and the compositor's own calls come in on it
    /// (`shellchan.rs`).
    pub(super) fn shell_endpoint(&self) -> Option<Endpoint> {
        self.shell.as_ref().map(|shell| shell.events)
    }

    /// Forget the attached compositor: close its event endpoint and remove
    /// the chords it registered (a re-attach registers them again).
    fn drop_shell(&mut self) {
        self.pointer.subscribed = false;
        // A hold is the compositor's: it ends with it.
        self.hold.set(false, [0; 4]);
        // So is the user's layout: back to the machine default.
        self.end_session_layout();
        if let Some(old) = self.shell.take() {
            let _ = old.events.close();
            for id in old.hotkeys {
                self.engine.remove_hotkey(id);
            }
        }
    }

    // ---- teardown ---------------------------------------------------------

    /// `session` is gone (closed, replaced, its surface or endpoint died):
    /// drop its endpoint, its key-state page and any grab it held.
    pub(super) fn forget_endpoint(&mut self, session: u64) {
        self.delivery.forget(session);
        self.key_pages.forget(session);
        self.grants_forget(session);
    }

    /// A session's endpoint died: drop it and tell the compositor.
    fn drop_session(&mut self, session: u64) {
        if let Some(removed) = self.router.remove(session) {
            self.forget_endpoint(session);
            if let Some(surface) = removed.surface {
                self.announce_closed(surface);
            }
        }
    }

    /// Tell the compositor `surface` has no session (legacy delivery resumes).
    pub(super) fn announce_closed(&mut self, surface: u64) {
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
pub(super) fn route_error(error: RouteError) -> Error {
    Error::Errno(-match error {
        RouteError::NoSurface | RouteError::NoSession => errno::ENOENT,
        RouteError::NotOwner => errno::EACCES,
        RouteError::Full => errno::ENOMEM,
        RouteError::Busy => errno::EBUSY,
    })
}

/// Whether `sender` holds the display grant: the compositor. The kernel says
/// who that is, so no client can pose as it by registering a name.
fn is_compositor(sender: u64) -> bool {
    sys::input_display_owner() == Ok(sender)
}
