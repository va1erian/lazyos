//! The scan table: BSSes by BSSID, with ageing and a capacity bound.
//!
//! Anyone in radio range can invent BSSIDs without limit, so the table never
//! grows past its capacity. When full, a new BSS replaces the weakest entry if
//! it is stronger than that entry (ties keep the older entry), otherwise it is
//! dropped. Entries are also removed by [`ScanTable::expire`] when not heard
//! for a while. A BSSID already in the table is always updated in place.

use alloc::vec::Vec;

use crate::bss::Bss;
use crate::Mac;

/// What [`ScanTable::update`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Update {
    Inserted,
    Updated,
    /// The table was full and the BSS was no stronger than any entry.
    Dropped,
}

#[derive(Clone, Debug)]
pub struct ScanTable {
    entries: Vec<Bss>,
    capacity: usize,
}

impl ScanTable {
    /// An empty table holding at most `capacity` BSSes.
    pub fn new(capacity: usize) -> ScanTable {
        ScanTable {
            entries: Vec::new(),
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn get(&self, bssid: &Mac) -> Option<&Bss> {
        self.entries.iter().find(|bss| &bss.bssid == bssid)
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Bss> {
        self.entries.iter()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Record a frame's BSS. A hidden SSID never overwrites a name already
    /// learned for the same BSSID (probe responses name hidden networks;
    /// the next beacon must not blank it).
    pub fn update(&mut self, mut bss: Bss) -> Update {
        if let Some(slot) = self.entries.iter_mut().find(|e| e.bssid == bss.bssid) {
            if bss.hidden && !slot.hidden {
                bss.ssid = core::mem::take(&mut slot.ssid);
                bss.hidden = false;
            }
            *slot = bss;
            return Update::Updated;
        }
        if self.entries.len() < self.capacity {
            self.entries.push(bss);
            return Update::Inserted;
        }
        let weakest = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| (e.rssi, e.last_seen))
            .map(|(index, e)| (index, e.rssi));
        match weakest {
            Some((index, rssi)) if bss.rssi > rssi => {
                self.entries[index] = bss;
                Update::Inserted
            }
            _ => Update::Dropped,
        }
    }

    /// Remove entries not heard for more than `max_age_ms` before `now_ms`.
    /// Returns how many went. A `now_ms` earlier than an entry's time counts
    /// as age 0.
    pub fn expire(&mut self, now_ms: u64, max_age_ms: u64) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|e| now_ms.saturating_sub(e.last_seen) <= max_age_ms);
        before - self.entries.len()
    }

    /// The table sorted strongest first (ties by BSSID, for a stable order).
    pub fn by_signal(&self) -> Vec<&Bss> {
        let mut sorted: Vec<&Bss> = self.entries.iter().collect();
        sorted.sort_by(|a, b| b.rssi.cmp(&a.rssi).then(a.bssid.cmp(&b.bssid)));
        sorted
    }
}
