//! Software scanning: the plan (which channels, for how long, probing or
//! listening) and the results (raw beacons and probe responses, unparsed).
//!
//! The radio steps channels under [`crate::SoftMac::poll`]: tune, send the
//! probe requests if the domain allows it, stay `dwell` milliseconds,
//! next. Every beacon or probe response heard meanwhile is kept as received
//! (the station manager parses it); a BSS heard again replaces its earlier
//! frame of the same kind, so the table holds the latest of each and stays
//! bounded however long a scan runs or however many BSSIDs a hostile radio
//! invents.

use alloc::vec::Vec;

use ieee80211::frame::{SUB_BEACON, SUB_PROBE_RESP};

use crate::radio::{Band, Channel};

/// Default dwell per channel when probing, and when only listening (a
/// little over one 102.4 ms beacon interval).
pub const DWELL_ACTIVE_MS: u32 = 50;
pub const DWELL_PASSIVE_MS: u32 = 120;
/// A requested dwell is clamped to this range.
pub const DWELL_MIN_MS: u32 = 10;
pub const DWELL_MAX_MS: u32 = 1000;
/// Most SSIDs one scan probes for, and their longest.
pub const MAX_SCAN_SSIDS: usize = 4;
pub const MAX_SSID: usize = 32;
/// Most channels one scan may name.
pub const MAX_SCAN_CHANNELS: usize = 64;
/// Results kept per scan, and their total size.
pub const MAX_RESULTS: usize = 256;
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
/// `ScanResults` returns at most this many entries per page.
pub const PAGE_MAX: usize = 64;

/// One channel visit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub channel: Channel,
    /// Send probe requests on arrival (active scan on a channel the domain
    /// lets us transmit on first).
    pub probe: bool,
    pub dwell_ms: u32,
}

/// A heard beacon or probe response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanEntry {
    pub frame: Vec<u8>,
    pub channel: u8,
    pub band: Band,
    pub rssi_dbm: i8,
    /// When it was heard (the soft-MAC's millisecond clock).
    pub heard_ms: u64,
}

/// A page of results as `ScanResults` returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<'a> {
    pub entries: Vec<(&'a ScanEntry, u32)>,
    pub more: bool,
}

#[derive(Debug)]
pub(crate) struct Running {
    pub(crate) id: u32,
    pub(crate) steps: Vec<Step>,
    /// The step being dwelt on, once the first was tuned.
    pub(crate) index: Option<usize>,
    pub(crate) until_ms: u64,
    pub(crate) ssids: Vec<Vec<u8>>,
}

#[derive(Debug, Default)]
pub(crate) struct Scanner {
    next_id: u32,
    pub(crate) running: Option<Running>,
    results_id: Option<u32>,
    entries: Vec<ScanEntry>,
    bytes: usize,
}

/// The BSSID and kind of a beacon or probe response (Address 3, subtype).
fn identity(frame: &[u8]) -> Option<([u8; 6], u8)> {
    let bssid = frame.get(16..22)?.try_into().ok()?;
    Some((bssid, frame.first()? >> 4))
}

impl Scanner {
    /// Begin a scan of `steps`; the previous results are dropped.
    pub(crate) fn start(&mut self, steps: Vec<Step>, ssids: Vec<Vec<u8>>) -> u32 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = self.next_id;
        self.running = Some(Running {
            id,
            steps,
            index: None,
            until_ms: 0,
            ssids,
        });
        self.results_id = Some(id);
        self.entries.clear();
        self.bytes = 0;
        id
    }

    pub(crate) fn running_id(&self) -> Option<u32> {
        self.running.as_ref().map(|r| r.id)
    }

    /// Keep a beacon or probe response heard on `channel`. Anything else, or
    /// a frame too short to name its BSS, is ignored.
    pub(crate) fn record(&mut self, frame: &[u8], band: Band, channel: u8, rssi_dbm: i8, now: u64) {
        if self.running.is_none() {
            return;
        }
        let Some((bssid, subtype)) = identity(frame) else {
            return;
        };
        if subtype != SUB_BEACON && subtype != SUB_PROBE_RESP {
            return;
        }
        let entry = ScanEntry {
            frame: Vec::from(frame),
            channel,
            band,
            rssi_dbm,
            heard_ms: now,
        };
        if let Some(old) = self
            .entries
            .iter_mut()
            .find(|e| identity(&e.frame) == Some((bssid, subtype)) && e.band == band)
        {
            if self.bytes - old.frame.len() + frame.len() <= MAX_RESULT_BYTES {
                self.bytes = self.bytes - old.frame.len() + frame.len();
                *old = entry;
            }
            return;
        }
        if self.entries.len() < MAX_RESULTS && self.bytes + frame.len() <= MAX_RESULT_BYTES {
            self.bytes += frame.len();
            self.entries.push(entry);
        }
    }

    /// The latest beacon or probe response of `bssid` in the current results.
    pub(crate) fn latest_of(&self, bssid: &[u8; 6]) -> Option<&ScanEntry> {
        self.entries
            .iter()
            .filter(|e| identity(&e.frame).is_some_and(|(b, _)| &b == bssid))
            .max_by_key(|e| e.heard_ms)
    }

    /// A page of `id`'s results from entry `first`: at most `max` entries
    /// (capped at [`PAGE_MAX`]) and at most `byte_budget` octets of frames
    /// (at least one entry when any is left, so paging always advances), each
    /// with its age at `now`. `None` for an unknown scan.
    pub(crate) fn page(
        &self,
        id: u32,
        first: u32,
        max: u32,
        byte_budget: usize,
        now: u64,
    ) -> Option<Page<'_>> {
        if self.results_id != Some(id) {
            return None;
        }
        let first = usize::try_from(first).unwrap_or(usize::MAX);
        let max = usize::try_from(max).unwrap_or(PAGE_MAX).min(PAGE_MAX);
        let mut entries = Vec::new();
        let mut used = 0usize;
        let mut index = first;
        while let Some(entry) = self.entries.get(index) {
            if entries.len() >= max
                || (!entries.is_empty() && used + entry.frame.len() > byte_budget)
            {
                break;
            }
            used += entry.frame.len();
            let age = u32::try_from(now.saturating_sub(entry.heard_ms)).unwrap_or(u32::MAX);
            entries.push((entry, age));
            index += 1;
        }
        Some(Page {
            entries,
            more: index < self.entries.len(),
        })
    }
}
