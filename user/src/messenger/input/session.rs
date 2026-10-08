//! A client key session on `inputd` for a native (`no_std`) reader: the
//! compositor's trusted prompt reads its keys this way.

use alloc::vec::Vec;

use super::{call_on, request, wire, INTERFACE, NAME};
use crate::messenger::{create_pair, errno, registry, Endpoint, Error, Result, DEFAULT_BUFFER};

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
    service: Endpoint,
    /// Whether `service` came from name resolution (released on close);
    /// otherwise it is the [`ShellLink`](super::ShellLink)'s private channel, which stays.
    resolved: bool,
    session: u64,
    /// This task's end of the session's event channel.
    events: Endpoint,
    buffer: Vec<u8>,
}

impl KeySession {
    /// Open a session for `surface`, which `inputd` must know as this task's.
    pub fn open(surface: u64) -> Result<KeySession> {
        let service = registry::resolve(NAME)?;
        let opened = Self::open_on(service, surface, true);
        if opened.is_err() {
            // A resolved handle: released, never closed (see `ShellLink::close`).
            let _ = service.release();
        }
        opened
    }

    pub(super) fn open_on(service: Endpoint, surface: u64, resolved: bool) -> Result<KeySession> {
        let (events, peer) = create_pair()?;
        let body = wire::encode_open_args(&wire::OpenArgs {
            surface: Some(surface),
        })
        .map_err(Error::Parcel);
        let (handles, _) = wire::encode_open_transfers(&wire::OpenTransfers {
            events: peer.handle(),
        });
        let reply = body.and_then(|body| {
            call_on(
                &service,
                &request(INTERFACE, wire::METHOD_OPEN, body, handles),
            )
        });
        match reply.and_then(|reply| wire::decode_open_reply(&reply.body).map_err(Error::Parcel)) {
            // Session ids start at 1; zero is a missing field.
            Ok(reply) if reply.session != 0 => Ok(KeySession {
                service,
                resolved,
                session: reply.session,
                events,
                buffer: alloc::vec![0u8; DEFAULT_BUFFER],
            }),
            other => {
                // The peer may or may not have moved; closing a stale handle
                // only fails harmlessly.
                let _ = peer.close();
                let _ = events.close();
                Err(other.err().unwrap_or(Error::Errno(-errno::EINVAL)))
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
            if message.interface_id() != INTERFACE {
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
        if let Ok(body) = wire::encode_close_args(&wire::CloseArgs {
            session: self.session,
        }) {
            let _ = call_on(
                &self.service,
                &request(INTERFACE, wire::METHOD_CLOSE, body, Vec::new()),
            );
        }
        if self.resolved {
            let _ = self.service.release();
        }
        let _ = self.events.close();
    }
}
