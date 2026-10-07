//! Ending a session's approvals with the session (review of #659): `elevd`
//! follows `logind`'s retained session records,
//! `system/events/login/session/<id>`, on `init`'s broker, and the policy in
//! `elevpolicy::sessions` says which ids are over (logged out, or reused by a
//! new session after `logind` restarted). Their standing approvals and
//! prompt holds go.
//!
//! The feed is read before each request is decided, so an approval is never
//! looked at with a record still unread. The records are retained: a
//! (re)subscription replays every session's last state.
//!
//! Serial: `ELEVD:SESSION:END session=<id>`.

use alloc::format;
use alloc::vec::Vec;

use elevpolicy::sessions::Sessions;
use user::messenger::{logind, router, services, DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

/// Ticks (100 Hz) between attempts to subscribe while `init` is unreachable.
const RETRY_TICKS: u64 = 300;
/// Most records read per look: the request waiting must not wait long.
const MAX_PER_LOOK: usize = 64;

/// Follows `system/events/login/session/+`.
pub(crate) struct Feed {
    sub: Option<router::Subscriber>,
    next_connect: u64,
    buffer: Vec<u8>,
    sessions: Sessions,
}

impl Feed {
    pub(crate) fn new() -> Feed {
        Feed {
            sub: None,
            next_connect: 0,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
            sessions: Sessions::new(),
        }
    }

    /// The session ids whose grants must end, from the records published
    /// since the last look.
    pub(crate) fn ended(&mut self) -> Vec<u64> {
        let mut ended = Vec::new();
        if self.sub.is_none() {
            let now = sys::clock();
            if now < self.next_connect {
                return ended;
            }
            self.next_connect = now + RETRY_TICKS;
            self.sub = router::Bus::connect(services::INIT_NAME)
                .and_then(|mut bus| {
                    logind::wire::subscribe_system_events_login_session(&mut bus, "+")
                })
                .ok();
        }
        for _ in 0..MAX_PER_LOOK {
            let Some(sub) = &self.sub else { break };
            match sub.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                Ok(Some(event)) => {
                    if let Some(id) = self.note(&event) {
                        sys::write_str(&format!("ELEVD:SESSION:END session={id}\n"));
                        ended.push(id);
                    }
                }
                Ok(None) => break,
                // The broker went away: subscribe again later (and replay).
                Err(_) => self.sub = None,
            }
        }
        ended
    }

    /// The session a record ends, if it ends one.
    fn note(&mut self, event: &router::Event) -> Option<u64> {
        let record = logind::wire::decode_system_events_login_session(&event.payload).ok()?;
        let id: u64 = event.topic.rsplit('/').next()?.parse().ok()?;
        self.sessions
            .record(id, &record.state, record.pid)
            .then_some(id)
    }
}
