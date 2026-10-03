//! Where every record goes: the in-memory ring, the persistent journals and
//! (for the first few) the serial console.

use alloc::format;

use user::sys;

use crate::journal::Journals;
use crate::ring::Ring;

/// Cap on records echoed to serial, so a crash loop cannot flood the log.
const PRINT_LIMIT: u64 = 24;

/// The ring, the journals and the serial echo budget.
pub(super) struct Log {
    pub(super) ring: Ring,
    pub(super) journals: Journals,
    printed: u64,
}

impl Log {
    pub(super) fn new(journals: Journals) -> Log {
        Log {
            ring: Ring::new(),
            journals,
            printed: 0,
        }
    }

    /// Append one record now.
    pub(super) fn append(&mut self, topic: &str, detail: &str) {
        let record = self.ring.append(sys::clock(), topic, detail);
        self.journals.record(&record);
        if self.printed < PRINT_LIMIT {
            sys::write_str(&format!(
                "logd: record {} {} {}\n",
                record.seq, record.topic, record.detail
            ));
            self.printed += 1;
        }
    }
}
