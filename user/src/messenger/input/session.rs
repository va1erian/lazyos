//! A client key session on `inputd` for a native (`no_std`) reader: the
//! compositor's trusted prompt reads its keys this way.

use alloc::vec::Vec;

use libmessenger::client::{CallError, Connection};

use super::{wire, CALL_BOUND, NAME};
use crate::messenger::client::{call_error, Held};
use crate::messenger::{create_pair, registry, Endpoint, Result, DEFAULT_BUFFER};

type CallResult<T> = core::result::Result<T, CallError>;

/// What a client session delivers that its reader acts on ([`KeySession::poll`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyInput {
    /// A key went down or repeats: its HID usage and the modifiers held.
    Down { code: u32, mods: u32 },
    /// The text a key produced under the active layout (composed).
    Text(alloc::string::String),
}

/// A client session on `inputd` for one surface, for a native (`no_std`)
/// reader: layout-aware keys and composed text from every keyboard, PS/2 or
/// USB, while that surface has the focus. The compositor's trusted prompt
/// reads its keys this way (docs/accounts-plan.md U2); xui apps carry their
/// own client.
pub struct KeySession {
    /// The generated client of `os.lazy.input.v1`, over the resolved endpoint
    /// (released with it) or the shell link's private channel (which stays).
    input: wire::Client<Held>,
    session: u64,
    /// This task's end of the session's event channel.
    events: Endpoint,
    buffer: Vec<u8>,
}

impl KeySession {
    /// Open a session for `surface`, which `inputd` must know as this task's.
    pub fn open(surface: u64) -> CallResult<KeySession> {
        let service = registry::resolve(NAME).map_err(call_error)?;
        // A resolved handle is released with the client, never closed.
        Self::open_on(Held::resolved(service), surface)
    }

    pub(super) fn open_on(held: Held, surface: u64) -> CallResult<KeySession> {
        let input = wire::Client::from_connection(Connection::new(held, NAME));
        let (events, peer) = create_pair().map_err(call_error)?;
        match input.open_within(Some(surface), peer.handle(), CALL_BOUND) {
            // Session ids start at 1; zero is a missing field.
            Ok(session) if session != 0 => Ok(KeySession {
                input,
                session,
                events,
                buffer: alloc::vec![0u8; DEFAULT_BUFFER],
            }),
            other => {
                // The peer may or may not have moved; closing a stale handle
                // only fails harmlessly.
                let _ = peer.close();
                let _ = events.close();
                Err(other
                    .err()
                    .unwrap_or(CallError::Transport(libmessenger::client::Errno::BADMSG)))
            }
        }
    }

    /// This task's end of the event channel, to park on.
    pub fn events_endpoint(&self) -> Endpoint {
        self.events
    }

    /// The next key or text, without blocking. `Err`: `inputd` went away.
    /// Key releases and anything else the session carries are skipped.
    pub fn poll(&mut self) -> Result<Option<KeyInput>> {
        loop {
            let Some(message) = self.events.poll_recv_with(&mut self.buffer)? else {
                return Ok(None);
            };
            if message.interface_id() != wire::INTERFACE_ID {
                continue;
            }
            let body = &message.parcel.body;
            match message.method() {
                wire::METHOD_KEYEVENT => match wire::decode_key_event_args(body) {
                    Ok(key) if key.state != wire::KEY_STATE_UP => {
                        return Ok(Some(KeyInput::Down {
                            code: key.code,
                            mods: key.mods,
                        }))
                    }
                    _ => {}
                },
                wire::METHOD_TEXTINPUT => {
                    if let Ok(text) = wire::decode_text_input_args(body) {
                        return Ok(Some(KeyInput::Text(text.utf8)));
                    }
                }
                _ => {}
            }
        }
    }

    /// End the session and release both endpoints (best effort: `inputd`
    /// also ends a session whose surface is unregistered).
    pub fn close(self) {
        let _ = self.input.close_within(self.session, CALL_BOUND);
        let _ = self.events.close();
    }
}
