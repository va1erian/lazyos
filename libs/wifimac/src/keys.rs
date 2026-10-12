//! The soft-MAC's view of the installed keys: which exist (the radio holds
//! the key material), the transmit packet number, and a replay window per
//! key. Key bytes never live here.

use ieee80211::ccmp::{Replay, PN_MAX};

/// The pairwise key: its key id and its receive window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pairwise {
    pub(crate) index: u8,
    pub(crate) replay: Replay,
}

/// Group keys by key id (1-3; slot 0 unused), and the transmit PN.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Keys {
    pub(crate) pairwise: Option<Pairwise>,
    pub(crate) group: [Option<Replay>; 4],
    /// The PN of the last frame sent; 0 before the first (the first is 1).
    /// It belongs to the association, not to one pairwise key: a rekey, or a
    /// key deleted and installed again, carries it on, so a temporal key
    /// installed twice (a reinstalled or repeated key) can never see a PN
    /// again. Only [`Keys::clear`] (the association ends) starts it over.
    tx_pn: u64,
}

impl Keys {
    pub(crate) fn any(&self) -> bool {
        self.pairwise.is_some() || self.group.iter().any(Option::is_some)
    }

    pub(crate) fn clear(&mut self) {
        *self = Keys::default();
    }

    /// The next transmit PN and the key id, without using it; `None` without
    /// a pairwise key or once the 48-bit space is spent (the key must be
    /// renewed, never wrapped). [`Keys::commit_tx`] uses it, once the frame
    /// is known to go.
    pub(crate) fn peek_tx(&self) -> Option<(u64, u8)> {
        let key = self.pairwise.as_ref()?;
        (self.tx_pn < PN_MAX).then(|| (self.tx_pn + 1, key.index))
    }

    /// Record `pn` (from [`Keys::peek_tx`]) as sent.
    pub(crate) fn commit_tx(&mut self, pn: u64) {
        self.tx_pn = self.tx_pn.max(pn);
    }

    /// The replay window a received protected frame is checked against:
    /// a group key by key id for a group-addressed receiver, the pairwise key
    /// (whose id must match) otherwise.
    pub(crate) fn window(&mut self, group_addressed: bool, key_id: u8) -> Option<&mut Replay> {
        if group_addressed {
            self.group.get_mut(usize::from(key_id))?.as_mut()
        } else {
            self.pairwise
                .as_mut()
                .filter(|key| key.index == key_id)
                .map(|key| &mut key.replay)
        }
    }
}
