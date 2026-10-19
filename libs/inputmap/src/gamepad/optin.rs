//! Which sessions asked for gamepad traffic (`docs/usb-gamepad-plan.md`,
//! decision 5).
//!
//! A pad's events (`GamepadAxis`, `GamepadButton`, and the `GamepadEnter` and
//! `GamepadLeave` that bracket them) go only to a focused session that called
//! `AttachGamepadState`. Every session has a 64-message endpoint shared with
//! its keystrokes, so a stick wiggled under a focused app that ignores pads
//! (an unknown method is dropped by the client) must not be able to overflow
//! it and force a keyboard resync: opting in is how an app says it reads pads.
//! The opt-in lasts until the session closes.

use alloc::collections::BTreeSet;

#[derive(Default)]
pub struct OptIn {
    sessions: BTreeSet<u64>,
}

impl OptIn {
    pub fn new() -> OptIn {
        OptIn::default()
    }

    /// `session` attached a pad page (again: no change).
    pub fn attach(&mut self, session: u64) {
        self.sessions.insert(session);
    }

    /// `session` closed.
    pub fn forget(&mut self, session: u64) {
        self.sessions.remove(&session);
    }

    /// Whether `session` opted in.
    pub fn has(&self, session: u64) -> bool {
        self.sessions.contains(&session)
    }

    /// Who receives a pad event while `focused` has the keyboard: that session
    /// if it opted in, nobody otherwise.
    pub fn recipient(&self, focused: Option<u64>) -> Option<u64> {
        focused.filter(|&session| self.has(session))
    }

    /// How many sessions opted in.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}
