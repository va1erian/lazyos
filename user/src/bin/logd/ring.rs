//! `logd`'s bounded, hash-chained record ring.
//!
//! Split out of `logd.rs` (issue #194) when the declared-topic payload
//! formatting moved to its own module.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// The journals (`libs/logstore`) chain their lines with the same function.
use logstore::line::record_hash;

/// Newest records kept in the ring.
pub(super) const RING_CAPACITY: usize = 64;

/// One hash-chained log record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) seq: u64,
    pub(super) tick: u64,
    pub(super) topic: String,
    pub(super) detail: String,
    pub(super) hash: u64,
}

/// The bounded ring and its chain head.
pub(super) struct Ring {
    records: Vec<Record>,
    /// Hash of the newest record (`0` before the first append).
    head: u64,
    /// Records appended since boot.
    pub(super) total: u64,
    /// Oldest records dropped when the ring wrapped.
    dropped: u64,
}

impl Ring {
    pub(super) fn new() -> Ring {
        Ring {
            records: Vec::new(),
            head: 0,
            total: 0,
            dropped: 0,
        }
    }

    /// The retained records, oldest first.
    pub(super) fn records(&self) -> &[Record] {
        &self.records
    }

    /// Append a record, extending the chain; returns the new record.
    pub(super) fn append(&mut self, tick: u64, topic: &str, detail: &str) -> Record {
        let seq = self.total + 1;
        let hash = record_hash(self.head, seq, tick, topic, detail);
        if self.records.len() >= RING_CAPACITY {
            self.records.remove(0);
            self.dropped += 1;
        }
        let record = Record {
            seq,
            tick,
            topic: topic.to_string(),
            detail: detail.to_string(),
            hash,
        };
        self.records.push(record.clone());
        self.head = hash;
        self.total = seq;
        record
    }

    /// Verify the retained window's chain links, and the genesis link while
    /// nothing has been dropped. Returns `(intact, first bad index)`; the index
    /// is the record count when the chain is intact.
    pub(super) fn verify(&self) -> (bool, u64) {
        let mut previous = 0u64;
        for (index, record) in self.records.iter().enumerate() {
            let expected = record_hash(
                previous,
                record.seq,
                record.tick,
                &record.topic,
                &record.detail,
            );
            // The first retained record's predecessor may have been dropped;
            // the window then starts trusted and every following link is
            // still checked.
            if record.hash != expected && !(index == 0 && self.dropped > 0) {
                return (false, index as u64);
            }
            previous = record.hash;
        }
        (true, self.records.len() as u64)
    }
}
