//! The prompt-flood brake (review of #659, H4).
//!
//! The trusted prompt takes every key and click while it is up, and a
//! cancelled or timed-out prompt costs the asker nothing, so a program could
//! ask again and again and the person at the screen would never reach Log
//! out. Two rules stop that:
//!
//! * **per asker**: after a prompt for a caller (uid, label, session; see
//!   [`Caller`]) was cancelled or timed out, that caller's requests are
//!   refused without a prompt for [`FIRST_HOLD_TICKS`], doubling with each
//!   further unanswered prompt up to [`MAX_HOLD_TICKS`]. An approval ends the
//!   hold and the count; so does a quiet [`FORGET_TICKS`].
//! * **for everyone**: no prompt opens for [`QUIET_TICKS`] after any prompt
//!   was cancelled or timed out, so two programs taking turns still leave
//!   the desktop reachable between their prompts.
//!
//! A wrong password is not counted here: `accountdb::ratelimit` already
//! slows those.

use alloc::vec::Vec;

use crate::approvals::Caller;

/// The first hold after an unanswered prompt (PIT ticks, 100 Hz): 5 s.
pub const FIRST_HOLD_TICKS: u64 = 5 * 100;
/// The longest hold: 2 minutes.
pub const MAX_HOLD_TICKS: u64 = 2 * 60 * 100;
/// The pause after any unanswered prompt before the next one: 3 s.
pub const QUIET_TICKS: u64 = 3 * 100;
/// A caller with no unanswered prompt for this long starts again from the
/// first hold: 10 minutes.
pub const FORGET_TICKS: u64 = 10 * 60 * 100;
/// Most callers tracked at once; the one whose hold ended longest ago goes.
pub const MAX_TRACKED: usize = 32;

/// Why a request may not prompt now, and until when (ticks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// This caller's prompts went unanswered.
    Caller { until: u64 },
    /// Some prompt was just cancelled: everybody waits a moment.
    Quiet { until: u64 },
}

impl Hold {
    /// The tick the hold ends.
    pub const fn until(self) -> u64 {
        match self {
            Hold::Caller { until } | Hold::Quiet { until } => until,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Strikes {
    caller: Caller,
    /// Unanswered prompts in a row.
    count: u32,
    until: u64,
}

/// The holds.
#[derive(Clone, Debug, Default)]
pub struct Backoff {
    entries: Vec<Strikes>,
    quiet_until: u64,
}

impl Backoff {
    pub fn new() -> Backoff {
        Backoff::default()
    }

    /// Whether `caller` may be shown a prompt at `now`: the caller's own hold
    /// comes first, then the pause everybody waits.
    pub fn check(&self, caller: Caller, now: u64) -> Result<(), Hold> {
        if let Some(entry) = self.find(caller) {
            if now < entry.until {
                return Err(Hold::Caller { until: entry.until });
            }
        }
        if now < self.quiet_until {
            return Err(Hold::Quiet {
                until: self.quiet_until,
            });
        }
        Ok(())
    }

    /// A prompt for `caller` was cancelled or timed out at `now`.
    pub fn unanswered(&mut self, caller: Caller, now: u64) {
        self.quiet_until = now.saturating_add(QUIET_TICKS);
        let count = match self.find(caller) {
            Some(entry) if now < entry.until.saturating_add(FORGET_TICKS) => entry.count,
            _ => 0,
        }
        .saturating_add(1);
        let hold = hold_ticks(count);
        self.entries.retain(|entry| entry.caller != caller);
        if self.entries.len() >= MAX_TRACKED {
            // The oldest hold goes; a caller still held outlives any that ended.
            if let Some(oldest) = (0..self.entries.len()).min_by_key(|&i| self.entries[i].until) {
                self.entries.remove(oldest);
            }
        }
        self.entries.push(Strikes {
            caller,
            count,
            until: now.saturating_add(hold),
        });
    }

    /// An administrator approved `caller`'s request: its hold and count end.
    pub fn approved(&mut self, caller: Caller) {
        self.entries.retain(|entry| entry.caller != caller);
    }

    /// Forget every hold of `session` (it ended).
    pub fn end_session(&mut self, session: u64) {
        self.entries.retain(|entry| entry.caller.session != session);
    }

    fn find(&self, caller: Caller) -> Option<&Strikes> {
        self.entries.iter().find(|entry| entry.caller == caller)
    }
}

/// The hold after `count` unanswered prompts in a row: 5 s, 10 s, 20 s, ...
/// up to [`MAX_HOLD_TICKS`].
pub fn hold_ticks(count: u32) -> u64 {
    let doublings = count.saturating_sub(1).min(16);
    FIRST_HOLD_TICKS
        .saturating_mul(1 << doublings)
        .min(MAX_HOLD_TICKS)
}
