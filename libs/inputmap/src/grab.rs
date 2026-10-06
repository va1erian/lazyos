//! Keyboard grabs (`docs/input-plan.md`, I3): who holds one, who asked.
//!
//! A focused session may ask for a keyboard grab (a fullscreen game, a remote
//! desktop): while it holds one, every key goes to it, the compositor's own
//! chords included. Grabs are never taken, only *granted*: the request waits
//! for the compositor's answer ([`Grabs::approve`]), and a grant needs the
//! session to still have focus at that moment. A grab ends when its holder
//! releases it, loses focus or closes, and always when the user presses the
//! reserved escape chord ([`Grabs::escape`]; the chord itself lives in
//! [`crate::Engine`], which nobody can register or grab).
//!
//! Pure bookkeeping, host-tested: `inputd` turns each [`Change`] into a
//! `GrantChanged` event for the client and a `GrabChanged` for the shell.

/// Why a grab (or a request) ended or started. The numeric values are the
/// wire's `GrantReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Approved = 0,
    Denied = 1,
    Released = 2,
    FocusLost = 3,
    Escaped = 4,
    Closed = 5,
}

/// One session's grant state changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub session: u64,
    /// The session holds the grab now.
    pub active: bool,
    pub reason: Reason,
    /// The grab itself (not just a pending request) began or ended, so the
    /// shell's view of who holds it changed.
    pub holder_changed: bool,
}

/// Why a request was refused outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The session does not have keyboard focus.
    NotFocused,
}

/// What [`Grabs::request`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requested {
    /// Waiting for the compositor; it must be asked. A request another
    /// session had pending was withdrawn ([`Change`] for it, if any).
    Pending { withdrawn: Option<Change> },
    /// The session already holds the grab: nothing to ask.
    AlreadyHeld,
}

#[derive(Debug, Default)]
pub struct Grabs {
    holder: Option<u64>,
    pending: Option<u64>,
}

impl Grabs {
    pub fn new() -> Grabs {
        Grabs::default()
    }

    /// The session holding the keyboard grab.
    pub fn holder(&self) -> Option<u64> {
        self.holder
    }

    /// The session whose request waits for the compositor.
    pub fn pending(&self) -> Option<u64> {
        self.pending
    }

    /// `session` asks for the grab; `focused` is the session with keyboard
    /// focus right now.
    pub fn request(&mut self, session: u64, focused: Option<u64>) -> Result<Requested, Refused> {
        if focused != Some(session) {
            return Err(Refused::NotFocused);
        }
        if self.holder == Some(session) {
            return Ok(Requested::AlreadyHeld);
        }
        // Only the focused session can ask, so an older request is stale.
        let withdrawn = self
            .pending
            .replace(session)
            .filter(|old| *old != session)
            .map(|old| denied(old, Reason::FocusLost));
        Ok(Requested::Pending { withdrawn })
    }

    /// The compositor's answer for `session`. `None` when no request of it
    /// is pending (`ENOENT`). Granted only while it still has focus.
    pub fn approve(&mut self, session: u64, allow: bool, focused: Option<u64>) -> Option<Change> {
        if self.pending != Some(session) {
            return None;
        }
        self.pending = None;
        if !allow || focused != Some(session) {
            return Some(denied(session, Reason::Denied));
        }
        self.holder = Some(session);
        Some(Change {
            session,
            active: true,
            reason: Reason::Approved,
            holder_changed: true,
        })
    }

    /// `session` gives up its grab or request. `None` when it had neither.
    pub fn release(&mut self, session: u64) -> Option<Change> {
        self.end(session, Reason::Released)
    }

    /// `session` closed (or its endpoint died).
    pub fn closed(&mut self, session: u64) -> Option<Change> {
        self.end(session, Reason::Closed)
    }

    /// Keyboard focus moved to `focused`: a holder or requester that lost
    /// it loses the grab.
    pub fn focus_changed(&mut self, focused: Option<u64>) -> Option<Change> {
        let pending = self.pending.filter(|s| Some(*s) != focused);
        let holder = self.holder.filter(|s| Some(*s) != focused);
        // At most one of them can be set: both need focus, and focus is one
        // session; report the grab over the request.
        let mut change = None;
        if let Some(session) = pending {
            change = self.end(session, Reason::FocusLost);
        }
        if let Some(session) = holder {
            change = self.end(session, Reason::FocusLost);
        }
        change
    }

    /// The escape chord: drop any grab and any request.
    pub fn escape(&mut self) -> Option<Change> {
        if let Some(session) = self.pending.take() {
            if self.holder.is_none() {
                return Some(denied(session, Reason::Escaped));
            }
        }
        let session = self.holder.take()?;
        Some(Change {
            session,
            active: false,
            reason: Reason::Escaped,
            holder_changed: true,
        })
    }

    fn end(&mut self, session: u64, reason: Reason) -> Option<Change> {
        if self.holder == Some(session) {
            self.holder = None;
            if self.pending == Some(session) {
                self.pending = None;
            }
            return Some(Change {
                session,
                active: false,
                reason,
                holder_changed: true,
            });
        }
        if self.pending == Some(session) {
            self.pending = None;
            return Some(denied(session, reason));
        }
        None
    }
}

/// A request that ended without a grab.
fn denied(session: u64, reason: Reason) -> Change {
    Change {
        session,
        active: false,
        reason,
        holder_changed: false,
    }
}
