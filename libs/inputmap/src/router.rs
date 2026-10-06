//! Who gets the keys: surfaces, sessions and keyboard focus.
//!
//! Pure bookkeeping for `inputd`. The compositor declares which task created
//! each surface ([`Router::register_surface`]) and which surface has focus
//! ([`Router::set_focus`]); a client then opens a session for a surface
//! ([`Router::open`]), which succeeds only for the surface's own creator, so a
//! task can never claim someone else's window. At most one session is
//! "focused" at any moment, and it is the only one that ever receives key
//! content.
//!
//! **The console session** (issue #396). One session may have no surface:
//! the login console's ([`Router::open_console`]; `inputd` admits only the
//! task holding the kernel's console claim). It is the fallback target: it
//! receives key content while no compositor is attached ([`Router::
//! set_compositor`]) and so nothing can have focus, which is exactly when the
//! console is what the screen shows. Under a compositor every key goes to a
//! window or nowhere, never to a prompt the user cannot see.

use alloc::collections::BTreeMap;

/// Most sessions one task may hold (a Files window per folder, say).
pub const MAX_SESSIONS_PER_OWNER: usize = 16;

/// Most sessions overall.
pub const MAX_SESSIONS: usize = 128;

/// Most surfaces the compositor may register.
pub const MAX_SURFACES: usize = 512;

/// Why a router call was refused. Maps onto errnos in `inputd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The surface is unknown (`ENOENT`).
    NoSurface,
    /// The surface belongs to another task (`EACCES`).
    NotOwner,
    /// A table is full (`ENOSPC`).
    Full,
    /// No such session (`ENOENT`).
    NoSession,
    /// Another task holds the console session (`EBUSY`).
    Busy,
}

/// One open input session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub owner: u64,
    /// The window it takes keys for; `None` for the console session.
    pub surface: Option<u64>,
}

/// What [`Router::open`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    pub session: u64,
    /// A previous session of the same surface, replaced by this one; the
    /// caller must drop its endpoint.
    pub replaced: Option<u64>,
    /// The surface already has keyboard focus, so the new session is entered
    /// immediately.
    pub focused: bool,
    /// The surface had no session before: the compositor should stop
    /// synthesising legacy key events for it.
    pub first_for_surface: bool,
}

/// The sessions affected by a focus change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusChange {
    /// Loses the keyboard (`KeyboardLeave`).
    pub left: Option<u64>,
    /// Gains the keyboard (`KeyboardEnter`).
    pub entered: Option<u64>,
}

#[derive(Default)]
pub struct Router {
    /// surface -> the task that created it.
    surfaces: BTreeMap<u64, u64>,
    sessions: BTreeMap<u64, Session>,
    /// surface -> its session.
    by_surface: BTreeMap<u64, u64>,
    focus: Option<u64>,
    next_session: u64,
    /// The console session, if one is open.
    console: Option<u64>,
    /// Whether a compositor is attached (the console then gets nothing).
    compositor: bool,
}

impl Router {
    pub fn new() -> Router {
        Router {
            next_session: 1,
            ..Router::default()
        }
    }

    /// Record that `owner` created `surface`. Re-registering updates the owner
    /// (a restarted compositor replays its table).
    pub fn register_surface(&mut self, surface: u64, owner: u64) -> Result<(), Error> {
        if !self.surfaces.contains_key(&surface) && self.surfaces.len() >= MAX_SURFACES {
            return Err(Error::Full);
        }
        self.surfaces.insert(surface, owner);
        Ok(())
    }

    /// Forget `surface` and close its session. Returns the closed session, if
    /// there was one.
    pub fn unregister_surface(&mut self, surface: u64) -> Option<u64> {
        self.surfaces.remove(&surface);
        if self.focus == Some(surface) {
            self.focus = None;
        }
        let session = self.by_surface.remove(&surface)?;
        self.sessions.remove(&session);
        Some(session)
    }

    /// Open the console session for `owner` (the caller checked the claim).
    /// Opening it again from the same task replaces it; from another task
    /// while it is open, [`Error::Busy`].
    pub fn open_console(&mut self, owner: u64) -> Result<Opened, Error> {
        let replaced = match self
            .console
            .and_then(|id| self.sessions.get(&id).map(|s| (id, s.owner)))
        {
            Some((_, holder)) if holder != owner => return Err(Error::Busy),
            Some((id, _)) => Some(id),
            None => None,
        };
        if replaced.is_none() && self.sessions.len() >= MAX_SESSIONS {
            return Err(Error::Full);
        }
        if let Some(old) = replaced {
            self.sessions.remove(&old);
        }
        let session = self.next_session;
        self.next_session += 1;
        self.sessions.insert(
            session,
            Session {
                owner,
                surface: None,
            },
        );
        self.console = Some(session);
        Ok(Opened {
            session,
            replaced,
            focused: self.focused_session() == Some(session),
            first_for_surface: false,
        })
    }

    /// The compositor attached (`true`) or went away. The console session
    /// loses or regains the keyboard accordingly.
    pub fn set_compositor(&mut self, attached: bool) -> FocusChange {
        let before = self.focused_session();
        self.compositor = attached;
        if !attached {
            self.focus = None;
        }
        change(before, self.focused_session())
    }

    /// Open a session for `owner` on `surface`.
    pub fn open(&mut self, owner: u64, surface: u64) -> Result<Opened, Error> {
        match self.surfaces.get(&surface) {
            None => return Err(Error::NoSurface),
            Some(&creator) if creator != owner => return Err(Error::NotOwner),
            Some(_) => {}
        }
        let replaced = self.by_surface.get(&surface).copied();
        let held = self
            .sessions
            .values()
            .filter(|s| s.owner == owner && s.surface.is_some())
            .count();
        let replacing_own = replaced.is_some();
        if !replacing_own && (held >= MAX_SESSIONS_PER_OWNER || self.sessions.len() >= MAX_SESSIONS)
        {
            return Err(Error::Full);
        }
        if let Some(old) = replaced {
            self.sessions.remove(&old);
        }
        let session = self.next_session;
        self.next_session += 1;
        self.sessions.insert(
            session,
            Session {
                owner,
                surface: Some(surface),
            },
        );
        self.by_surface.insert(surface, session);
        Ok(Opened {
            session,
            replaced,
            focused: self.focus == Some(surface),
            first_for_surface: replaced.is_none(),
        })
    }

    /// Close `session`, which must belong to `owner`. Returns its surface
    /// (`None` for the console session).
    pub fn close(&mut self, session: u64, owner: u64) -> Result<Option<u64>, Error> {
        match self.sessions.get(&session) {
            None => return Err(Error::NoSession),
            Some(found) if found.owner != owner => return Err(Error::NoSession),
            Some(_) => {}
        }
        let removed = self.remove(session).ok_or(Error::NoSession)?;
        Ok(removed.surface)
    }

    /// Drop `session` regardless of owner (its endpoint died). Returns it.
    pub fn remove(&mut self, session: u64) -> Option<Session> {
        let removed = self.sessions.remove(&session)?;
        if self.console == Some(session) {
            self.console = None;
        }
        if let Some(surface) = removed.surface {
            if self.by_surface.get(&surface) == Some(&session) {
                self.by_surface.remove(&surface);
            }
        }
        Some(removed)
    }

    /// The compositor moved focus to `surface` (`None`: nothing focused).
    pub fn set_focus(&mut self, surface: Option<u64>) -> FocusChange {
        if self.focus == surface {
            return FocusChange::default();
        }
        let before = self.focused_session();
        self.focus = surface;
        change(before, self.focused_session())
    }

    /// The session that receives key content right now: the focused
    /// window's, or with no compositor attached, the console session.
    pub fn focused_session(&self) -> Option<u64> {
        match self.focus {
            Some(surface) => self.by_surface.get(&surface).copied(),
            None if !self.compositor => self.console,
            None => None,
        }
    }

    /// The console session, if one is open.
    pub fn console(&self) -> Option<u64> {
        self.console
    }

    pub fn session(&self, session: u64) -> Option<Session> {
        self.sessions.get(&session).copied()
    }

    /// Every open session id.
    pub fn sessions(&self) -> impl Iterator<Item = u64> + '_ {
        self.sessions.keys().copied()
    }

    /// The surface that has a session, if any (for the compositor's legacy
    /// suppression).
    pub fn has_session(&self, surface: u64) -> bool {
        self.by_surface.contains_key(&surface)
    }
}

/// The leave/enter pair for a move of the keyboard from `before` to `after`.
fn change(before: Option<u64>, after: Option<u64>) -> FocusChange {
    if before == after {
        return FocusChange::default();
    }
    FocusChange {
        left: before,
        entered: after,
    }
}
