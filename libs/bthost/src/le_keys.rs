//! What the host believes about LE keys: which identity a pairing may take,
//! and the stored key a controller asks for.

use bthci::{BdAddr, Command};

use crate::host::Host;

impl Host {
    /// Whether a pairing on the link `old` may end in the identity `addr`. An
    /// identity nobody is bonded under is free. One that an LE bond already
    /// carries belongs to that device: only a pairing started for exactly it
    /// (the link was made to it, the user named it, or the harness list does)
    /// may replace the bond.
    pub(crate) fn identity_may_replace(&self, old: BdAddr, addr: BdAddr) -> bool {
        if !self.store.iter().any(|b| b.addr == addr && b.kind.is_le()) {
            return true;
        }
        let named = self.ctl.pairing.phase.active()
            && [self.ctl.pairing.addr, self.ctl.identity_alias].contains(&Some(addr));
        old == addr || named || self.cfg.le_targets.contains(&addr)
    }

    /// The controller asks for the key of a link (the peripheral role; the
    /// host is always the central here, but a controller may still ask): the
    /// stored LTK, only if the EDIV and Rand are the ones it was issued under.
    pub(crate) fn on_ltk_request(&mut self, handle: u16, random: u64, ediv: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return self.cmd(Command::LeLongTermKeyRequestNegativeReply { handle });
        };
        let addr = self.links[idx].addr;
        let key = self
            .store
            .iter()
            .find(|b| b.addr == addr && b.kind.is_le())
            .and_then(|b| b.ltk.as_ref())
            .filter(|k| k.rand == random && k.ediv == ediv)
            .map(|k| k.key);
        self.cmd(match key {
            Some(ltk) => Command::LeLongTermKeyRequestReply { handle, ltk },
            None => Command::LeLongTermKeyRequestNegativeReply { handle },
        });
    }
}
