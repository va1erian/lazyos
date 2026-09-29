//! Clipboard state: the per-session offer tables, their lookups and the
//! service counters. The request paths that mutate this state live in
//! [`crate::handlers`].

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use user::central;
use user::messenger::{clipboard as wire, errno, Endpoint, Error};

use super::MAX_SESSIONS;

/// One live offer in a session.
pub(super) struct Offer {
    pub(super) token: u64,
    pub(super) owner: String,
    pub(super) session: u64,
    pub(super) owner_slot: u64,
    pub(super) mimes: Vec<String>,
    pub(super) data: Vec<(String, Vec<u8>)>,
    pub(super) sink: Option<String>,
    /// Cached owner endpoint once a lazy paste resolved the sink.
    pub(super) endpoint: Option<Endpoint>,
    pub(super) lazy: bool,
    pub(super) tick: u64,
}

impl Offer {
    /// Metadata only: the shape `Current` and the changed topic carry.
    pub(super) fn info(&self) -> wire::OfferInfo {
        wire::OfferInfo {
            token: self.token,
            owner: self.owner.clone(),
            session: self.session,
            mimes: self.mimes.clone(),
            lazy: self.lazy,
            tick: self.tick,
        }
    }
}

/// One session's offer history; newest last.
pub(super) struct SessionClip {
    pub(super) session: u64,
    pub(super) offers: VecDeque<Offer>,
}

/// The clipboard state: the per-session tables, the central-broker connection
/// the changed topic and audit events publish through, and the counters the
/// paste log reports.
pub(super) struct Clipboard {
    pub(super) sessions: Vec<SessionClip>,
    pub(super) history: usize,
    pub(super) next_token: u64,
    /// Cached `messengerd` connection; `None` until connected (or after a
    /// publish failure, when the next event reconnects).
    pub(super) central: Option<central::Bus>,
    pub(super) pastes: u64,
    pub(super) denies: u64,
}

impl Clipboard {
    /// An empty clipboard with `history` offers kept per session.
    pub(super) fn new(history: usize) -> Clipboard {
        Clipboard {
            sessions: Vec::new(),
            history,
            next_token: 0,
            central: None,
            pastes: 0,
            denies: 0,
        }
    }

    /// Find the session row, creating it (up to [`MAX_SESSIONS`]).
    pub(super) fn session_index(&mut self, session: u64) -> Result<usize, Error> {
        if let Some(index) = self
            .sessions
            .iter()
            .position(|clip| clip.session == session)
        {
            return Ok(index);
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(Error::Errno(-errno::ENOMEM));
        }
        self.sessions.push(SessionClip {
            session,
            offers: VecDeque::new(),
        });
        Ok(self.sessions.len() - 1)
    }

    /// The session's newest offer, metadata only.
    pub(super) fn current(&self, session: u64) -> Option<wire::OfferInfo> {
        let clip = self.sessions.iter().find(|clip| clip.session == session)?;
        clip.offers.back().map(Offer::info)
    }
}
