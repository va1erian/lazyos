//! Logouts (issue #623): `xuid` follows `logind`'s `system/events/login/end`
//! on `init`'s broker and, when the session that owns the display ends, lets
//! the next desktop session's LazyShell claim the shell role.
//!
//! The shell role is never granted by uid (`protocol::privileged`): it goes
//! to a task of the session that owns the display, the first login session
//! whose task subscribed as the shell. Without this feed that session would
//! own the display for the rest of the boot and a second login's shell would
//! be refused. A forged end event (the broker accepts any publisher) can only
//! clear the owner; while the owner's shell is alive nobody else may claim the
//! role anyway (`shellcalls::shell_allowed`), and `logind` and `init` end the
//! whole session at logout before the next one opens.
//!
//! Serial: `XUID:LOGOUT session=<id>` when the display owner's session ended.

use alloc::vec::Vec;
use user::messenger::{logind, router, services, DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

/// Ticks (100 Hz) between looks at the topic.
const POLL_TICKS: u64 = 25;
/// Ticks between attempts to subscribe while `init` is unreachable.
const RETRY_TICKS: u64 = 300;

/// Follows `system/events/login/end`.
pub(super) struct LoginFeed {
    sub: Option<router::Subscriber>,
    next_poll: u64,
    next_connect: u64,
    buffer: Vec<u8>,
}

impl LoginFeed {
    pub(super) fn new() -> LoginFeed {
        LoginFeed {
            sub: None,
            next_poll: 0,
            next_connect: 0,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        }
    }

    /// The sessions that ended since the last poll.
    pub(super) fn poll(&mut self) -> Vec<u64> {
        let now = sys::clock();
        let mut ended = Vec::new();
        if now < self.next_poll {
            return ended;
        }
        self.next_poll = now + POLL_TICKS;
        if self.sub.is_none() {
            if now < self.next_connect {
                return ended;
            }
            self.next_connect = now + RETRY_TICKS;
            self.sub = router::Bus::connect(services::INIT_NAME)
                .and_then(|mut bus| logind::wire::subscribe_system_events_login_end(&mut bus))
                .ok();
        }
        while let Some(sub) = &self.sub {
            match sub.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                Ok(Some(event)) => {
                    if let Ok(end) = logind::wire::decode_system_events_login_end(&event.payload) {
                        ended.push(end.session);
                    }
                }
                Ok(None) => break,
                // The broker went away: subscribe again later.
                Err(_) => self.sub = None,
            }
        }
        ended
    }
}
