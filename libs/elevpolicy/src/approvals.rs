//! Standing approvals: an administrator's `conf.*` approval covers the same
//! caller for [`APPROVAL_TICKS`](crate::APPROVAL_TICKS), so an elevated
//! settings editor does not prompt for every key it reads or writes.
//!
//! An approval belongs to the exact kernel-stamped caller that asked: uid,
//! label and session. Another program of the same user (a different label)
//! or the same program in another session gets none, and a logout ends the
//! session it was granted to. `Release` drops the caller's approvals at
//! once; the table is bounded and expired entries are dropped first.

use alloc::vec::Vec;

use crate::{Class, APPROVAL_TICKS};

/// Most approvals held at once.
pub const MAX_APPROVALS: usize = 16;

/// The kernel-stamped identity an approval is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub label: u32,
    pub session: u64,
}

#[derive(Clone, Copy, Debug)]
struct Approval {
    caller: Caller,
    until: u64,
}

/// The standing approvals.
#[derive(Clone, Debug, Default)]
pub struct Approvals {
    entries: Vec<Approval>,
}

impl Approvals {
    pub fn new() -> Approvals {
        Approvals::default()
    }

    /// Whether `caller` holds a live approval for an operation of `class`.
    pub fn covers(&self, caller: Caller, class: Class, now: u64) -> bool {
        class == Class::Conf
            && self
                .entries
                .iter()
                .any(|entry| entry.caller == caller && now < entry.until)
    }

    /// Record an administrator's approval for `caller`, when its class
    /// stands ([`Class::Conf`]); a renewed approval restarts the clock.
    pub fn grant(&mut self, caller: Caller, class: Class, now: u64) {
        if class != Class::Conf {
            return;
        }
        self.entries
            .retain(|entry| entry.caller != caller && now < entry.until);
        if self.entries.len() >= MAX_APPROVALS {
            self.entries.remove(0);
        }
        self.entries.push(Approval {
            caller,
            until: now.saturating_add(APPROVAL_TICKS),
        });
    }

    /// Drop `caller`'s approvals.
    pub fn release(&mut self, caller: Caller) {
        self.entries.retain(|entry| entry.caller != caller);
    }

    /// Drop every approval of `session` (it ended).
    pub fn end_session(&mut self, session: u64) {
        self.entries.retain(|entry| entry.caller.session != session);
    }

    /// How many approvals stand at `now`.
    pub fn live(&self, now: u64) -> usize {
        self.entries
            .iter()
            .filter(|entry| now < entry.until)
            .count()
    }
}
