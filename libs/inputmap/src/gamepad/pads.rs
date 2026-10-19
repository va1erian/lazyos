//! The classifier and the engine as `inputd` drives them: one object that
//! learns which devices are pads from the kernel and applies their records.
//!
//! The glue between [`route::Classifier`] (who is a pad) and [`Gamepad`] (what
//! a pad is doing) is here rather than in `inputd` so that host tests and the
//! fuzz entry exercise exactly what the service runs.
//!
//! **The source table is read ahead of the stream.** `inputd` reads the
//! kernel's table *now*, but the records published before that read can still
//! be queued in its ring, and the table already reflects what they said (a pad
//! pressed and then unplugged is gone from the table while its press, release
//! and `SOURCE` 0 are yet to come). Applying it at once would make the
//! classifier forget the pad early and give its queued buttons to the cursor.
//! So [`Pads::table`] only *holds* a table with the kernel's `next_seq` (every
//! record with a smaller `seq` was published before the read), and it is
//! applied at its place in the stream: by [`Pads::reach`] just before the first
//! record with `seq >= next_seq` is handled, or by [`Pads::settle`] when the
//! drain ran dry first. `SOURCE` records before that point update the
//! classifier as usual. A merged pointer or axis record keeps its older `seq`,
//! but the kernel never merges across a `SOURCE`, so such a record cannot
//! carry a device over a change of the table.
//!
//! A `Dropped` marker breaks the stream: a held table whose `next_seq` is at or
//! below the marker's `seq` is applied before it (everything it covers has
//! been handled); any other is replaced by the fresh read the caller makes
//! after it. Records between the marker and that read are classified from the
//! stream alone, which is wrong only for a device whose `SOURCE` records were
//! lost in the gap (a known limit, `docs/usb-gamepad-plan.md`).

use alloc::vec::Vec;

use super::route::{Change, Classifier, Route, SourceEntry, GAMEPAD_CLASS};
use super::{Gamepad, PadOut, RawPad};

/// What happened to a pad's slot, for the service's trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `device` is a pad and has a slot.
    Attached(u8),
    /// `device` is a pad but all slots are taken: its records are dropped
    /// until a slot comes free.
    Refused(u8),
    /// `device` stopped being a pad (or its source was replaced); its buttons
    /// and axes were released.
    Detached(u8),
}

/// A source table read, waiting for the stream to reach it.
struct Held {
    entries: Vec<SourceEntry>,
    /// The bus's next sequence number when it was read.
    next_seq: u64,
}

#[derive(Default)]
pub struct Pads {
    classifier: Classifier,
    engine: Gamepad,
    /// Reused between calls.
    changes: Vec<Change>,
    /// The newest table read, not yet applied.
    held: Option<Held>,
    /// `next_seq` of the table applied last (0: none yet).
    applied: u64,
    /// The timestamp of the newest record handled, for stamping a release a
    /// table causes at the end of a drain.
    last_ts_ns: u64,
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
    ///
    /// Fail safe while a table is held: a device the classifier knows nothing
    /// of, but which the held table lists as a gamepad, is a pad whose
    /// `SOURCE` record was lost, so its records are dropped rather than given
    /// to the cursor (a missed press beats a stray click). A device the stream
    /// has classified keeps its class: records before the table's cut are
    /// that source's, whatever the table says of its successor.
    pub fn route(&self, device: u8, kind: u8) -> Route {
        match self.classifier.route(device, kind) {
            Route::Pointer if self.held_pad(device) => Route::Drop,
            route => route,
        }
    }

    fn held_pad(&self, device: u8) -> bool {
        !self.classifier.is_known(device)
            && self.held.as_ref().is_some_and(|held| {
                held.entries
                    .iter()
                    .any(|e| e.device == device && e.class == GAMEPAD_CLASS)
            })
    }

    /// A record [`route`](Pads::route) sent to [`Route::Pad`].
    pub fn feed(&mut self, record: RawPad, out: &mut Vec<PadOut>) -> bool {
        self.engine.apply(record, out)
    }

    /// A kernel `SOURCE` record.
    pub fn source(&mut self, record: RawPad, out: &mut Vec<PadOut>, notices: &mut Vec<Notice>) {
        let mut changes = core::mem::take(&mut self.changes);
        self.last_ts_ns = record.ts_ns;
        self.classifier.source_record(&record, &mut changes);
        self.apply(&mut changes, record.ts_ns, record.seq, out, notices);
        self.changes = changes;
    }

    /// Hold the source table the kernel just reported, read when the bus opens
    /// and after every `Dropped`. `next_seq` is the sequence number the kernel
    /// returned with it: nothing is applied until the stream reaches it
    /// ([`Pads::reach`], [`Pads::settle`]). A table already held is replaced:
    /// this one is newer.
    pub fn table(&mut self, entries: &[SourceEntry], next_seq: u64) {
        self.held = Some(Held {
            entries: entries.to_vec(),
            next_seq,
        });
    }

    /// Whether a table waits for the stream.
    pub fn has_held_table(&self) -> bool {
        self.held.is_some()
    }

    /// `next_seq` of the table applied last, 0 before the first.
    pub fn applied_table(&self) -> u64 {
        self.applied
    }

    /// The stream is about to handle the record `seq` (`ts_ns`): if a held
    /// table covers everything before it (`seq >= next_seq`), apply the table
    /// first. Returns whether one was applied. Call it before routing or
    /// feeding every pad, pointer, `AXIS` and `SOURCE` record and before a
    /// `Dropped` marker.
    pub fn reach(
        &mut self,
        seq: u64,
        ts_ns: u64,
        out: &mut Vec<PadOut>,
        notices: &mut Vec<Notice>,
    ) -> bool {
        self.last_ts_ns = ts_ns;
        match &self.held {
            Some(held) if seq >= held.next_seq => self.apply_held(out, notices),
            _ => false,
        }
    }

    /// The drain ran dry (or the bus just opened): everything queued before the
    /// held table's read has been handled, so apply it. Returns whether one
    /// was held.
    pub fn settle(&mut self, out: &mut Vec<PadOut>, notices: &mut Vec<Notice>) -> bool {
        self.apply_held(out, notices)
    }

    fn apply_held(&mut self, out: &mut Vec<PadOut>, notices: &mut Vec<Notice>) -> bool {
        let Some(held) = self.held.take() else {
            return false;
        };
        let mut changes = core::mem::take(&mut self.changes);
        self.classifier.table(&held.entries, &mut changes);
        // The table reflects every record below `next_seq`: a release it
        // causes is stamped with the newest of them.
        let seq = held.next_seq.saturating_sub(1);
        self.applied = held.next_seq;
        let ts_ns = self.last_ts_ns;
        self.apply(&mut changes, ts_ns, seq, out, notices);
        self.changes = changes;
        true
    }

    /// The end of a bus drain: coalesced axis changes go out.
    pub fn flush(&mut self, out: &mut Vec<PadOut>) {
        self.engine.flush(out);
    }

    /// The bus lost records: release and centre every pad. The caller then
    /// reads the source table again ([`Pads::table`]).
    pub fn resync(&mut self, ts_ns: u64, seq: u64, out: &mut Vec<PadOut>) {
        self.last_ts_ns = ts_ns;
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
        let mut freed = false;
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
                        freed = true;
                    }
                }
            }
        }
        if freed {
            self.attach_waiting(notices);
        }
    }

    /// A slot came free: give it to the classified pads that were refused for
    /// want of one, lowest device id first. `PadAttached` fires only when a
    /// device's class changes, so without this a refused pad would stay
    /// silent until it was replugged.
    fn attach_waiting(&mut self, notices: &mut Vec<Notice>) {
        for device in self.classifier.pad_devices() {
            if self.engine.pad(device).is_none() && self.engine.attach(device) {
                notices.push(Notice::Attached(device));
            }
        }
    }
}
