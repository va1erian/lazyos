//! Which login sessions are still the ones approvals were granted to
//! (review of #659): `elevd` follows `logind`'s retained session records
//! (`system/events/login/session/<id>`) and ends a session's standing
//! approvals and prompt holds when its record says it is over.
//!
//! Session ids start again from 1 when `logind` restarts, so a later session
//! can carry the id of one that held an approval. A record that is not
//! `active` (`starting`, `exited`) ends the id, and so does an `active`
//! record whose shell task differs from the one seen before: a new session
//! under an old id never inherits anything. A forged record (the broker
//! takes any publisher) can only end approvals early, never grant one.

use alloc::vec::Vec;

/// The `state` of a live session's record.
pub const ACTIVE: &str = "active";
/// Most sessions remembered; the oldest is forgotten first.
pub const MAX_SESSIONS: usize = 64;

/// The shell task seen for each live session.
#[derive(Clone, Debug, Default)]
pub struct Sessions {
    seen: Vec<(u64, u64)>,
}

impl Sessions {
    pub fn new() -> Sessions {
        Sessions::default()
    }

    /// Note session `id`'s record (`state`, shell task `pid`). `true` when
    /// whatever was granted under `id` must end now.
    pub fn record(&mut self, id: u64, state: &str, pid: u64) -> bool {
        let known = self.seen.iter().position(|(seen, _)| *seen == id);
        if state != ACTIVE {
            if let Some(at) = known {
                self.seen.remove(at);
            }
            return true;
        }
        match known {
            Some(at) if self.seen[at].1 == pid => false,
            Some(at) => {
                self.seen[at].1 = pid;
                true
            }
            None => {
                if self.seen.len() >= MAX_SESSIONS {
                    self.seen.remove(0);
                }
                self.seen.push((id, pid));
                false
            }
        }
    }
}
