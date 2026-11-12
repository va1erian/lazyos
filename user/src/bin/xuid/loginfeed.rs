//! Logouts (issue #623): `xuid` follows `logind`'s retained session records,
//! `system/events/login/session/<id>`, on the central broker and, when the
//! session that owns the display has exited, lets the next desktop session's
//! LazyShell claim the shell role.
//!
//! The shell role is never granted by uid (`protocol::privileged`): it goes
//! to a task of the session that owns the display, the first login session
//! whose task subscribed as the shell. Without this feed that session would
//! own the display for the rest of the boot and a second login's shell would
//! be refused. A forged record (the broker accepts any publisher) can only
//! clear the owner; while the owner's shell is alive nobody else may claim the
//! role anyway (`shellcalls::shell_allowed`), and `logind` and `init` end the
//! whole session at logout before the next one opens.
//!
//! The records are retained, so a (re)subscription replays every session's
//! last state: a logout published while the subscription was down (the
//! broker restarted) is still seen, rather than leaving a stale owner that
//! refuses every later login's shell. `system/events/login/end` is a one-off
//! event with no such replay, which is why this feed does not use it.
//!
//! Serial: `XUID:LOGOUT session=<id>` when the display owner's session ended.

use alloc::vec::Vec;
use user::central::Feed;
use user::messenger::logind;
use user::sys;

/// Ticks (100 Hz) between looks at the doorbell.
const POLL_TICKS: u64 = 25;
/// The `state` of a session record whose session has ended.
const EXITED: &str = "exited";

/// Follows `system/events/login/session/+`.
pub(super) struct LoginFeed {
    feed: Feed<logind::wire::LoginSession>,
    next_poll: u64,
}

impl LoginFeed {
    pub(super) fn new() -> LoginFeed {
        LoginFeed {
            feed: Feed::new(|bus| logind::wire::subscribe_system_events_login_session(bus, "+")),
            next_poll: 0,
        }
    }

    /// The sessions seen exited since the last poll (a replayed record of a
    /// session that ended long ago included: forgetting an owner that is
    /// already gone is harmless).
    pub(super) fn poll(&mut self) -> Vec<u64> {
        let now = sys::clock();
        let mut ended = Vec::new();
        if now < self.next_poll {
            return ended;
        }
        self.next_poll = now + POLL_TICKS;
        self.feed.ensure();
        self.feed.drain_rung(|event| {
            if event.value.state == EXITED {
                ended.extend(
                    event
                        .topic
                        .rsplit('/')
                        .next()
                        .and_then(|id| id.parse::<u64>().ok()),
                );
            }
        });
        ended
    }
}
