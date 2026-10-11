//! The scan table merged with the known profiles, as one caller may see it
//! (`os.lazy.net.wifi.v1` `Networks`).

use alloc::string::String;
use alloc::vec::Vec;

use ieee80211::Bss;

use crate::machine::{Phase, Station};
use crate::select::classify;
use crate::ssid;
use crate::types::{Band, Owner, Profile, Security};
use crate::MAX_NETWORKS;

/// Who asks: whose profile ids may be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Viewer {
    /// A user: the system's profiles and their own.
    User(u32),
    /// A system service (`elevd`): the system's profiles only.
    System,
}

impl Viewer {
    /// Whether this viewer may see (and use) `owner`'s profile.
    pub fn sees(self, owner: Owner) -> bool {
        match (owner, self) {
            (Owner::System, _) => true,
            (Owner::User(uid), Viewer::User(me)) => uid == me,
            (Owner::User(_), Viewer::System) => false,
        }
    }
}

/// One heard network (`os.lazy.net.wifi.v1` `Network`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    /// Sanitised; empty for a hidden network (one entry per such BSS).
    pub ssid: String,
    pub security: Security,
    /// Of the strongest BSS.
    pub rssi_dbm: i32,
    pub freq_mhz: u32,
    pub bss_count: u32,
    /// The viewer's usable profile for it; empty when none.
    pub id: String,
    pub connected: bool,
}

struct Group<'a> {
    ssid: &'a [u8],
    hidden: bool,
    security: Security,
    best: &'a Bss,
    band: Band,
    count: u32,
}

impl<N: FnMut() -> [u8; 32] + Clone> Station<N> {
    /// The networks heard, strongest first, at most [`MAX_NETWORKS`].
    pub fn networks(&self, viewer: Viewer) -> Vec<Network> {
        let mut groups: Vec<Group<'_>> = Vec::new();
        for bss in self.table.by_signal() {
            let Some(&band) = self.bands.get(&bss.bssid) else {
                continue;
            };
            let (security, _) = classify(bss);
            let same = |g: &&mut Group<'_>| {
                !g.hidden && !bss.hidden && g.ssid == bss.ssid.as_slice() && g.security == security
            };
            if let Some(group) = groups.iter_mut().find(|g| same(g)) {
                // `by_signal` is strongest first, so the first BSS stays best.
                group.count += 1;
                continue;
            }
            groups.push(Group {
                ssid: &bss.ssid,
                hidden: bss.hidden,
                security,
                best: bss,
                band,
                count: 1,
            });
        }
        let connected = match &self.phase {
            Phase::Connected(link) => Some((link.cand.profile.ssid.as_slice(), link.cand.profile.security)),
            _ => None,
        };
        groups
            .iter()
            .take(MAX_NETWORKS)
            .map(|g| {
                let known = (!g.hidden).then(|| self.profile_for(g.ssid, g.security, viewer)).flatten();
                Network {
                    ssid: if g.hidden { String::new() } else { ssid::display(g.ssid) },
                    security: g.security,
                    rssi_dbm: i32::from(g.best.rssi),
                    freq_mhz: g.band.freq_mhz(g.best.channel),
                    bss_count: g.count,
                    id: known.map(|p| p.id.clone()).unwrap_or_default(),
                    connected: !g.hidden && connected == Some((g.ssid, g.security)),
                }
            })
            .collect()
    }

    /// The viewer's best profile for a network: highest priority.
    fn profile_for(&self, ssid: &[u8], security: Security, viewer: Viewer) -> Option<&Profile> {
        self.profiles
            .iter()
            .filter(|p| p.ssid == ssid && p.security == security && viewer.sees(p.owner))
            .max_by_key(|p| p.priority)
    }
}
