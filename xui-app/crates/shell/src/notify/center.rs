//! The notification center's model (docs/notify-plan.md): which toasts are
//! on screen, which wait, when each leaves, and the animation state the
//! presenter reads. There is no history: a toast that is closed is gone.
//!
//! * **Placement.** At most [`VISIBLE_MAX`] toasts show, of which one sender
//!   has at most [`OWNER_VISIBLE_MAX`], so a flood from one app leaves room
//!   for the others. The rest wait (at most [`WAITING_MAX`]); a burst of
//!   [`COALESCE_AT`] plain notices from one sender merges into one "N
//!   notifications" summary instead of stacking windows.
//! * **Replace in place.** `replaces_id` naming the sender's own live or
//!   waiting toast updates it and keeps its id and its place.
//! * **Time.** All times are ticks (100 Hz), supplied by the caller. A
//!   toast's life starts when it takes its place on screen; hovering pauses
//!   it. Warn, Error and unfinished progress never expire.
//! * **Motion.** Each placed toast has a position tween (a spring in, an
//!   eased reflow, a sink out) and an eased progress value, read as
//!   [`Frame`]s; `sys/ui/motion` is applied here, once.
//!
//! Every outcome a sender can hear of is queued as an [`Event`] and drained
//! with [`Center::take_events`], so the model never talks to anything.

mod queue;
mod toast;
mod view;

use std::collections::VecDeque;

use lazymotion::Motion;

use super::note::{CloseReason, Note, Request};
use super::policy::{Policy, RateLimit, SYSTEM_OWNER};

use toast::Leaving;
pub use toast::Toast;

/// Milliseconds per tick.
pub const TICK_MS: u64 = 10;
/// Most toasts on screen at once.
pub const VISIBLE_MAX: usize = 4;
/// Most of those from one sender.
pub const OWNER_VISIBLE_MAX: usize = 3;
/// Most toasts waiting for a place.
pub const WAITING_MAX: usize = 32;
/// Most toasts (shown and waiting) one sender has alive.
pub const OWNER_ALIVE_MAX: usize = 16;
/// This many waiting plain notices from one sender become one summary.
pub const COALESCE_AT: usize = 3;
/// Distance of the stack from the screen's right edge, between toasts, and
/// above the taskbar.
pub const MARGIN: i32 = 12;
pub const STACK_GAP: i32 = 8;
pub const BAR_GAP: i32 = 8;
/// How far below its place a toast starts, and ends.
pub const SLIDE_PX: i32 = 36;
/// Reflow, sink-out and progress-fill durations.
pub const REFLOW_MS: u32 = 220;
pub const LEAVE_MS: u32 = 180;
pub const PROGRESS_MS: u32 = 250;

/// Why a `Notify` was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The sender is posting faster than the rate limit (`EBUSY`).
    RateLimited,
    /// The sender has [`OWNER_ALIVE_MAX`] toasts alive (`ENOSPC`).
    Full,
}

/// What the center tells the presenter and the senders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub owner: String,
    pub id: u64,
    pub kind: EventKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// The toast took a place on screen (the tray item pulses).
    Shown,
    /// The user clicked its body.
    Activated,
    /// The user pressed action `n`.
    Action(u32),
    /// It is gone.
    Closed(CloseReason),
}
/// The notification center.
#[derive(Debug, Default)]
pub struct Center {
    live: Vec<Toast>,
    waiting: VecDeque<Toast>,
    leaving: Vec<Leaving>,
    next_id: u64,
    limits: RateLimit,
    policy: Policy,
    motion: Motion,
    events: Vec<Event>,
}

impl Center {
    pub fn new() -> Center {
        Center {
            next_id: 1,
            ..Center::default()
        }
    }

    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
    }

    pub fn motion(&self) -> Motion {
        self.motion
    }

    /// A sender's `Notify`: the toast's id. Held back by the user's policy
    /// (the toast is accepted, never shown, and closes `Muted`).
    pub fn post(&mut self, owner: &str, request: Request, now: u64) -> Result<u64, Refused> {
        if let Some(id) = self.replace(owner, &request, now) {
            return Ok(id);
        }
        if self.alive_of(owner) >= OWNER_ALIVE_MAX {
            return Err(Refused::Full);
        }
        if !self.limits.take(owner, now) {
            return Err(Refused::RateLimited);
        }
        let id = self.fresh_id();
        if self.policy.suppresses(owner, request.note.level) {
            self.emit(owner, id, EventKind::Closed(CloseReason::Muted));
            return Ok(id);
        }
        self.waiting.push_back(Toast::new(id, owner, request.note));
        self.evict_waiting();
        self.promote(now);
        Ok(id)
    }

    /// The shell's own toast: no rate limit, no policy, never evicted by a
    /// sender's flood of waiting ones.
    pub fn post_system(&mut self, request: Request, now: u64) -> u64 {
        if let Some(id) = self.replace(SYSTEM_OWNER, &request, now) {
            return id;
        }
        let id = self.fresh_id();
        self.waiting
            .push_back(Toast::new(id, SYSTEM_OWNER, request.note));
        self.evict_waiting();
        self.promote(now);
        id
    }

    /// The sender closed its toast (`Close`). Not an error for one that is
    /// gone or another sender's: `false` then.
    pub fn close(&mut self, owner: &str, id: u64, now: u64) -> bool {
        if self.find(id).is_none_or(|toast| toast.owner != owner) {
            return false;
        }
        self.remove(id, CloseReason::Sender, now);
        true
    }

    /// The user dismissed it.
    pub fn dismiss(&mut self, id: u64, now: u64) {
        self.remove(id, CloseReason::User, now);
    }

    /// The user clicked its body: the sender hears `Activated`, it closes.
    pub fn activate(&mut self, id: u64, now: u64) {
        if let Some(owner) = self.find(id).map(|toast| toast.owner.clone()) {
            self.emit(&owner, id, EventKind::Activated);
            self.remove(id, CloseReason::User, now);
        }
    }

    /// The user pressed action `action`; one the toast does not have is
    /// ignored.
    pub fn invoke(&mut self, id: u64, action: u32, now: u64) {
        let Some(toast) = self.find(id) else {
            return;
        };
        if !toast.note.actions.iter().any(|a| a.id == action) {
            return;
        }
        let owner = toast.owner.clone();
        self.emit(&owner, id, EventKind::Action(action));
        self.remove(id, CloseReason::User, now);
    }

    /// The pointer rests on (or left) toast `id`: its life pauses.
    pub fn hover(&mut self, id: u64, on: bool, now: u64) {
        let Some(toast) = self.live.iter_mut().find(|toast| toast.id == id) else {
            return;
        };
        if toast.hovered == on {
            return;
        }
        toast.hovered = on;
        if on {
            if let Some(deadline) = toast.deadline.take() {
                toast.left = Some(deadline.saturating_sub(now));
            }
        } else if let Some(left) = toast.left.take() {
            toast.deadline = Some(now.saturating_add(left));
        }
    }

    /// The presenter measured toast `id` at `height` design pixels for
    /// revision `rev`; a stale revision is ignored.
    pub fn set_height(&mut self, id: u64, rev: u32, height: i32) {
        if let Some(toast) = self.live.iter_mut().find(|toast| toast.id == id) {
            if toast.rev == rev {
                toast.height = height.max(1);
            }
        }
    }

    /// Advance time: expire toasts whose life ended, drop finished
    /// sink-outs, give freed places to the waiting.
    pub fn tick(&mut self, now: u64) {
        let now_ms = now * TICK_MS;
        self.leaving.retain(|gone| !gone.y.done(now_ms));
        let due: Vec<u64> = self
            .live
            .iter()
            .filter(|toast| toast.deadline.is_some_and(|deadline| deadline <= now))
            .map(|toast| toast.id)
            .collect();
        for id in due {
            self.remove(id, CloseReason::Timeout, now);
        }
        self.promote(now);
    }

    /// Whether nothing is shown, waiting or sinking.
    pub fn is_idle(&self) -> bool {
        self.live.is_empty() && self.waiting.is_empty() && self.leaving.is_empty()
    }

    /// The toasts on screen, oldest first.
    pub fn live(&self) -> &[Toast] {
        &self.live
    }

    /// How many toasts wait for a place.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Toast `id`, shown or waiting.
    pub fn get(&self, id: u64) -> Option<&Toast> {
        self.find(id)
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The center after a shell restart: every toast closes `Restart`.
    pub fn restart(&mut self, now: u64) {
        let ids: Vec<u64> = self
            .live
            .iter()
            .map(|toast| toast.id)
            .chain(self.waiting.iter().map(|toast| toast.id))
            .collect();
        for id in ids {
            self.remove(id, CloseReason::Restart, now);
        }
    }
}
