//! The classifier and the engine as `inputd` drives them: one object that
//! learns which devices are pads from the kernel and applies their records.
//!
//! The glue between [`route::Classifier`] (who is a pad) and [`Gamepad`] (what
//! a pad is doing) is here rather than in `inputd` so that host tests and the
//! fuzz entry exercise exactly what the service runs.

use alloc::vec::Vec;

use super::route::{Change, Classifier, Route, SourceEntry};
use super::{Gamepad, PadOut, RawPad};

/// What happened to a pad's slot, for the service's trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `device` is a pad and has a slot.
    Attached(u8),
    /// `device` is a pad but all slots are taken: its records are dropped.
    Refused(u8),
    /// `device` stopped being a pad (or its source was replaced); its buttons
    /// and axes were released.
    Detached(u8),
}

#[derive(Default)]
pub struct Pads {
    classifier: Classifier,
    engine: Gamepad,
    /// Reused between calls.
    changes: Vec<Change>,
}

impl Pads {
    pub fn new() -> Pads {
        Pads::default()
    }

    /// The pad engine, for its state ([`Gamepad::pads`], the page).
    pub fn engine(&self) -> &Gamepad {
        &self.engine
    }

    /// The classifier, for its counters.
    pub fn classifier(&self) -> &Classifier {
        &self.classifier
    }

    /// Where a `BUTTON`, `AXIS` or pointer record of `device` goes.
    pub fn route(&self, device: u8, kind: u8) -> Route {
        self.classifier.route(device, kind)
    }

    /// A record [`route`](Pads::route) sent to [`Route::Pad`].
    pub fn feed(&mut self, record: RawPad, out: &mut Vec<PadOut>) -> bool {
        self.engine.apply(record, out)
    }

    /// A kernel `SOURCE` record.
    pub fn source(&mut self, record: RawPad, out: &mut Vec<PadOut>, notices: &mut Vec<Notice>) {
        let mut changes = core::mem::take(&mut self.changes);
        self.classifier.source_record(&record, &mut changes);
        self.apply(&mut changes, record.ts_ns, record.seq, out, notices);
        self.changes = changes;
    }

    /// The live source table, read when the bus opens and after every
    /// `Dropped` (`ts_ns`/`seq` of that record stamp any release it causes).
    pub fn table(
        &mut self,
        entries: &[SourceEntry],
        ts_ns: u64,
        seq: u64,
        out: &mut Vec<PadOut>,
        notices: &mut Vec<Notice>,
    ) {
        let mut changes = core::mem::take(&mut self.changes);
        self.classifier.table(entries, &mut changes);
        self.apply(&mut changes, ts_ns, seq, out, notices);
        self.changes = changes;
    }

    /// The end of a bus drain: coalesced axis changes go out.
    pub fn flush(&mut self, out: &mut Vec<PadOut>) {
        self.engine.flush(out);
    }

    /// The bus lost records: release and centre every pad. The caller then
    /// re-reads the source table ([`Pads::table`]).
    pub fn resync(&mut self, ts_ns: u64, seq: u64, out: &mut Vec<PadOut>) {
        self.engine.resync(ts_ns, seq, out);
    }

    /// Records dropped as malformed, by the engine and the classifier.
    pub fn rejected(&self) -> u64 {
        self.engine.rejected() + self.classifier.rejected()
    }

    fn apply(
        &mut self,
        changes: &mut Vec<Change>,
        ts_ns: u64,
        seq: u64,
        out: &mut Vec<PadOut>,
        notices: &mut Vec<Notice>,
    ) {
        for change in changes.drain(..) {
            match change {
                Change::PadAttached(device) => notices.push(if self.engine.attach(device) {
                    Notice::Attached(device)
                } else {
                    Notice::Refused(device)
                }),
                Change::PadDetached(device) => {
                    if self.engine.pad(device).is_some() {
                        self.engine.detach(device, ts_ns, seq, out);
                        notices.push(Notice::Detached(device));
                    }
                }
            }
        }
    }
}
