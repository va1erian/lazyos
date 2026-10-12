//! What the soft-MAC keeps about the joined BSS: its channel and the
//! association's progress, what its beacons say (QoS, beacon interval,
//! whether it is protected), the last sequence numbers heard from it, and the
//! sequence counters of what we send it.

use ieee80211::frame::{Body, Mgmt};
use ieee80211::ie::{Elements, WPA_OUI};
use ieee80211::{cap, Mac};

use crate::radio::Channel;

/// The beacon interval assumed until one is heard (100 TU).
pub(crate) const DEFAULT_INTERVAL_MS: u64 = 102;
/// A beacon interval is believed between these (a beacon claiming 65535 TU
/// would put the beacon-loss watch a quarter of an hour away).
const MIN_INTERVAL_MS: u64 = 10;
const MAX_INTERVAL_MS: u64 = 1000;

/// The joined BSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bss {
    pub(crate) bssid: Mac,
    pub(crate) channel: Channel,
    pub(crate) aid: u16,
    /// The AP speaks QoS (HT or WMM in its beacon): data goes as QoS data.
    pub(crate) qos: bool,
    pub(crate) interval_ms: u64,
    pub(crate) last_beacon: u64,
    pub(crate) loss_reported: bool,
    /// A key was installed since the join: data must be protected from now
    /// on, both ways, even if the pairwise key is deleted again.
    pub(crate) protected: bool,
    /// The BSS advertised privacy or an RSN (or WPA) element, in the scan
    /// entry the join learned from or in a beacon since: the port opens only
    /// with a pairwise key. Never cleared for the join's life, so a forged
    /// open beacon cannot open the port without a key.
    pub(crate) privacy: bool,
    /// The BSS was heard on its channel (a scan entry heard there recently,
    /// or a beacon since the join): on a channel the domain lists as passive,
    /// nothing is transmitted before that.
    pub(crate) heard: bool,
    /// What the owner sent since the join (or since the AP ended the
    /// association), so a response is believed only after its request.
    pub(crate) sent_auth: bool,
    pub(crate) sent_assoc: bool,
    /// An association response was delivered: EAPOL may follow before the
    /// owner has moved the state to `Associated`.
    pub(crate) assoc_answered: bool,
    pub(crate) dups: Dups,
}

impl Bss {
    pub(crate) fn new(bssid: Mac, channel: Channel, traits: Traits, now: u64) -> Bss {
        Bss {
            bssid,
            channel,
            aid: 0,
            qos: traits.qos,
            interval_ms: traits.interval_ms,
            last_beacon: now,
            loss_reported: false,
            protected: false,
            privacy: traits.privacy,
            heard: false,
            sent_auth: false,
            sent_assoc: false,
            assoc_answered: false,
            dups: Dups::default(),
        }
    }

    /// The association ended: what was sent and answered no longer counts.
    pub(crate) fn reset_exchange(&mut self) {
        self.aid = 0;
        self.protected = false;
        self.sent_auth = false;
        self.sent_assoc = false;
        self.assoc_answered = false;
        self.dups = Dups::default();
    }
}

/// What a beacon or probe response says that the soft-MAC uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Traits {
    pub(crate) qos: bool,
    pub(crate) interval_ms: u64,
    pub(crate) privacy: bool,
}

impl Traits {
    /// What is assumed of a BSS nothing was heard from.
    pub(crate) const UNKNOWN: Traits = Traits {
        qos: false,
        interval_ms: DEFAULT_INTERVAL_MS,
        privacy: false,
    };
}

/// The [`Traits`] of a beacon or probe response, through the fuzzed parser.
pub(crate) fn bss_traits(frame: &[u8]) -> Option<Traits> {
    let mgmt = Mgmt::parse(frame).ok()?;
    let (Body::Beacon(b) | Body::ProbeResponse(b)) = mgmt.body else {
        return None;
    };
    let ies = Elements::parse(b.ies).ok()?;
    let wmm = ies.vendor.iter().any(|v| v.oui == WPA_OUI && v.kind == 2);
    let interval = u64::from(b.interval) * 1024 / 1000;
    let privacy = b.capability & cap::PRIVACY != 0 || ies.rsn.is_some() || ies.wpa_ie.is_some();
    Some(Traits {
        qos: ies.ht_cap.is_some() || wmm,
        interval_ms: interval.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS),
        privacy,
    })
}

/// The Retry bit of the frame control field's flags octet.
const RETRY: u8 = 0x08;

/// The last sequence control heard from the BSS per class (management,
/// non-QoS data, QoS data per TID), for dropping retransmissions of
/// unprotected unicast frames (IEEE 802.11-2020 10.3.2.14). A protected data
/// frame is the replay window's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Dups {
    mgmt: Option<u16>,
    data: [Option<u16>; 17],
}

impl Dups {
    /// Whether a unicast frame with `flags` and `sequence` (the whole
    /// sequence control field) repeats the last of its class; remembers it.
    /// `tid` is `None` for management, `Some(None)` for non-QoS data.
    pub(crate) fn repeat(&mut self, tid: Option<Option<u8>>, flags: u8, sequence: u16) -> bool {
        let last = match tid {
            None => &mut self.mgmt,
            Some(None) => &mut self.data[16],
            Some(Some(tid)) => &mut self.data[usize::from(tid & 0x0F)],
        };
        let repeat = flags & RETRY != 0 && *last == Some(sequence);
        *last = Some(sequence);
        repeat
    }
}

/// Sequence counters: one for management and non-QoS data, one per TID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sequences {
    plain: u16,
    qos: [u16; 16],
}

impl Sequences {
    pub(crate) fn next(&mut self, tid: Option<u8>) -> u16 {
        let counter = match tid {
            Some(tid) => &mut self.qos[usize::from(tid & 0x0F)],
            None => &mut self.plain,
        };
        let value = *counter;
        *counter = (*counter + 1) & 0x0FFF;
        value
    }
}
