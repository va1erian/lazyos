//! Ending a session's approvals with the session (review of #659): `elevd`
//! follows `logind`'s retained session records,
//! `system/events/login/session/<id>`, on the central broker, and the policy
//! in `elevpolicy::sessions` says which ids are over (logged out, or reused by
//! a new session after `logind` restarted). Their standing approvals and
//! prompt holds go.
//!
//! The feed is read before each request is decided, so an approval is never
//! looked at with a record still unread: it asks the feed's doorbell (a local
//! queue, never the broker) and reads only when it rang. The records are
//! retained: a (re)subscription replays every session's last state.
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
        let mut ended = Vec::new();
        self.feed.ensure();
        let sessions = &mut self.sessions;
        self.feed.drain_rung(|event| {
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
        });
        ended
    }
}
