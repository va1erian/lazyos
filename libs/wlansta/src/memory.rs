//! What failed attempts leave behind, so the station neither hammers a
//! network that cannot work nor lets one forged frame lock a network out:
//!
//! * a **block** on a profile concluded to have a wrong passphrase: it is not
//!   auto-joined until the user connects to it again or [`BLOCK_MS`] passes
//!   (a forged deauthentication can cause one, so it must decay);
//! * **strikes** on a profile: handshakes that sent message 2 and then ended
//!   without proof either way (the deadline, the AP leaving with a reason that
//!   says nothing). [`STRIKES`] of them in a row are a wrong passphrase;
//! * a short **penalty** on a BSSID that refused us, never answered or went
//!   silent: selection prefers another BSS of the SSID, or a fresh scan,
//!   before trying it again.
//!
//! Every list is bounded: blocks and strikes by the profiles (gone profiles
//! are dropped, [`Memory::retain`]), penalties by [`MAX_PENALTIES`].

use alloc::string::String;
use alloc::vec::Vec;

use crate::types::{Mac, Owner, Profile};

/// How long a wrong-passphrase block holds.
pub const BLOCK_MS: u64 = 15 * 60_000;
/// Inconclusive handshake failures in a row that make a wrong passphrase.
pub const STRIKES: u8 = 2;
/// How long a strike counts: older ones are not "in a row".
pub const STRIKE_MS: u64 = 10 * 60_000;
/// How long a BSSID that failed us is avoided.
pub const PENALTY_MS: u64 = 60_000;
/// BSSIDs penalised at once; the oldest goes first.
pub const MAX_PENALTIES: usize = 16;

struct Mark {
    owner: Owner,
    id: String,
    until: u64,
    count: u8,
}

impl Mark {
    fn is(&self, owner: Owner, id: &str) -> bool {
        self.owner == owner && self.id == id
    }
}

#[derive(Default)]
pub(crate) struct Memory {
    blocks: Vec<Mark>,
    strikes: Vec<Mark>,
    penalties: Vec<(Mac, u64)>,
}

impl Memory {
    /// Block `owner`'s profile `id` (a wrong passphrase); its strikes go.
    pub fn block(&mut self, owner: Owner, id: &str, now: u64) {
        self.unblock(owner, id);
        self.blocks.push(Mark {
            owner,
            id: String::from(id),
            until: now.saturating_add(BLOCK_MS),
            count: 0,
        });
    }

    pub fn blocked(&self, owner: Owner, id: &str) -> bool {
        self.blocks.iter().any(|m| m.is(owner, id))
    }

    /// Forget the block and the strikes of a profile (the user asked for it,
    /// or it worked).
    pub fn unblock(&mut self, owner: Owner, id: &str) {
        self.blocks.retain(|m| !m.is(owner, id));
        self.clear_strikes(owner, id);
    }

    /// Count one inconclusive handshake failure; returns the strikes now.
    pub fn strike(&mut self, owner: Owner, id: &str, now: u64) -> u8 {
        let until = now.saturating_add(STRIKE_MS);
        if let Some(mark) = self.strikes.iter_mut().find(|m| m.is(owner, id)) {
            mark.count = mark.count.saturating_add(1);
            mark.until = until;
            return mark.count;
        }
        self.strikes.push(Mark {
            owner,
            id: String::from(id),
            until,
            count: 1,
        });
        1
    }

    pub fn clear_strikes(&mut self, owner: Owner, id: &str) {
        self.strikes.retain(|m| !m.is(owner, id));
    }

    pub fn penalize(&mut self, bssid: Mac, now: u64) {
        self.penalties.retain(|(b, _)| *b != bssid);
        if self.penalties.len() >= MAX_PENALTIES {
            self.penalties.remove(0);
        }
        self.penalties.push((bssid, now.saturating_add(PENALTY_MS)));
    }

    pub fn penalized(&self, bssid: &Mac, now: u64) -> bool {
        self.penalties
            .iter()
            .any(|(b, until)| b == bssid && now < *until)
    }

    /// Drop what is about profiles that no longer exist.
    pub fn retain(&mut self, profiles: &[Profile]) {
        let known = |m: &Mark| profiles.iter().any(|p| p.is(m.owner, &m.id));
        self.blocks.retain(known);
        self.strikes.retain(known);
    }

    /// When the earliest block lifts (the only expiry that needs a tick:
    /// auto-connect may then try that profile again).
    pub fn next_expiry(&self) -> Option<u64> {
        self.blocks.iter().map(|m| m.until).min()
    }

    /// Forget whatever has expired; `true` when a block lifted.
    pub fn expire(&mut self, now: u64) -> bool {
        let before = self.blocks.len();
        self.blocks.retain(|m| now < m.until);
        self.strikes.retain(|m| now < m.until);
        self.penalties.retain(|(_, until)| now < *until);
        self.blocks.len() != before
    }

    /// Sizes, for the fuzz invariants.
    #[cfg(any(test, feature = "fuzz"))]
    pub fn sizes(&self) -> (usize, usize, usize) {
        (self.blocks.len(), self.strikes.len(), self.penalties.len())
    }
}
