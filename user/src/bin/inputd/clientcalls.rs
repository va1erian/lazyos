//! `os.lazy.input.v1` requests: open and close sessions, state, grabs, the
//! key-state page and `Ping`. Key content itself is pushed (`hub.rs`).

use alloc::format;
use alloc::vec::Vec;

use inputmap::{REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS};
use user::messenger::input::{shell_wire, wire};
use user::messenger::{errno, Endpoint, Error, Message, Result};
use user::sys;

use super::hub::{release_transfers, route_error, Hub};

/// PIT ticks are 10 ms.
const TICK_MS: u32 = 10;

impl Hub {
    pub(super) fn client_call(&mut self, message: &Message) -> Result<Vec<u8>> {
        let body = &message.parcel.body;
        match message.method() {
            wire::METHOD_OPEN => self.open(message),
            // It adopts (or releases) the buffer it carries itself.
            wire::METHOD_ATTACHKEYSTATE => self.attach_key_state(message),
            method => {
                release_transfers(message);
                match method {
                    wire::METHOD_CLOSE => self.close(message),
                    wire::METHOD_GETSTATE => wire::encode_get_state_reply(&wire::GetStateReply {
                        layout: self.engine.layout().name().into(),
                        mods: self.engine.mods(),
                        repeat_delay_ms: REPEAT_DELAY_TICKS as u32 * TICK_MS,
                        repeat_interval_ms: REPEAT_INTERVAL_TICKS as u32 * TICK_MS,
                    })
                    .map_err(Error::Parcel),
                    wire::METHOD_REQUESTGRANT => self.request_grant(message),
                    wire::METHOD_RELEASEGRANT => self.release_grant(message),
                    wire::METHOD_PING => {
                        let args = wire::decode_ping_args(body).map_err(Error::Parcel)?;
                        let session = self
                            .router
                            .session(args.session)
                            .ok_or(Error::Errno(-errno::ENOENT))?;
                        // The live sequence number reveals when keys are typed:
                        // only the session they are typed into may read it.
                        if session.owner != message.sender {
                            return Err(Error::Errno(-errno::EACCES));
                        }
                        let seq = if self.router.focused_session() == Some(args.session) {
                            self.engine.last_seq()
                        } else {
                            0
                        };
                        wire::encode_ping_reply(&wire::PingReply {
                            token: args.token,
                            seq,
                        })
                        .map_err(Error::Parcel)
                    }
                    _ => Err(Error::Errno(-errno::EINVAL)),
                }
            }
        }
    }

    /// `Close`: only the session's owner may.
    fn close(&mut self, message: &Message) -> Result<Vec<u8>> {
        let args = wire::decode_close_args(&message.parcel.body).map_err(Error::Parcel)?;
        let surface = self
            .router
            .close(args.session, message.sender)
            .map_err(route_error)?;
        self.forget_endpoint(args.session);
        if let Some(surface) = surface {
            self.announce_closed(surface);
        }
        Ok(Vec::new())
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
        if !message.carries(wire::OPEN_TRANSFERS) {
            return Err(Error::Errno(-errno::EINVAL));
        }
        // A session without a surface is the login console's (issue #396).
        let Some(surface) = args.surface else {
            return self.open_console(message);
        };
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
            "INPUTD:SESSION:OPEN session={} surface={surface} owner={}\n",
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
}
