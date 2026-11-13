//! Ending a session's approvals with the session (review of #659): `elevd`
//! follows `logind`'s retained session records,
//! `system/events/login/session/<id>`, on the central broker, and the policy
//! in `elevpolicy::sessions` says which ids are over (logged out, or reused by
//! a new session after `logind` restarted). Their standing approvals and
//! prompt holds go.
//!
//! The feed is read before each request is decided, so an approval is never
//! looked at with a record still unread: it reads what the doorbell says is
//! queued, without waiting. That can miss a record the broker has answered to
//! `logind` but not yet rung the bell for, so a request that a standing
//! approval would decide reads once more, waiting a tick
//! ([`SessionFeed::ended_waiting`]); a request that prompts anyway pays
//! nothing. The records are retained: a (re)subscription replays every
//! session's last state.
//!
//! Serial: `ELEVD:SESSION:END session=<id>`.

use alloc::vec::Vec;
use user::trace;

use elevpolicy::sessions::Sessions;
use user::central::Feed;
use user::messenger::logind;

/// Follows `system/events/login/session/+`.
pub(crate) struct SessionFeed {
    feed: Feed<logind::wire::LoginSession>,
    sessions: Sessions,
}

impl SessionFeed {
    pub(crate) fn new() -> SessionFeed {
        SessionFeed {
            feed: Feed::new(|bus| logind::wire::subscribe_system_events_login_session(bus, "+")),
            sessions: Sessions::new(),
        }
    }

    /// The session ids whose grants must end, from the records published
    /// since the last look.
    pub(crate) fn ended(&mut self) -> Vec<u64> {
        self.read(None)
    }

    /// [`SessionFeed::ended`], but the last pull waits up to `ticks` for a
    /// record the broker is still delivering: for a decision a standing
    /// approval would make, which must not use the approval of a session
    /// whose end is in flight.
    pub(crate) fn ended_waiting(&mut self, ticks: u64) -> Vec<u64> {
        self.read(Some(ticks))
    }

    fn read(&mut self, wait: Option<u64>) -> Vec<u64> {
        let mut ended = Vec::new();
        self.feed.ensure();
        let sessions = &mut self.sessions;
        let apply = |event: messenger_generated::topics::Event<logind::wire::LoginSession>| {
            // `system/events/login/session/<id>`: the id is the last segment.
            let Some(id) = event
                .topic
                .rsplit('/')
                .next()
                .and_then(|id| id.parse::<u64>().ok())
            else {
                return;
            };
            if sessions.record(id, &event.value.state, event.value.pid) {
                trace::line!(trace::Channel::Elevd, "SESSION:END session={id}");
                ended.push(id);
            }
        };
        match wait {
            Some(ticks) => self.feed.drain_waiting(ticks, apply),
            None => self.feed.drain_rung(apply),
        }
        ended
    }
}
