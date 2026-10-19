//! Byte-script fuzzing of the gamepad path: the classifier, the engine and the
//! routing between them, against a model of the kernel's source table and of
//! the client that receives the events.
//!
//! [`run`] reads a script of bus records, `SOURCE` registrations and closes
//! (some of them *lost*, as in a ring overflow), table reads, loss markers and
//! hostile records, and feeds them to a [`Pads`] the way `inputd`'s loop does,
//! with a real cursor engine on the other side of the routing. The same
//! function is the libFuzzer target (`fuzz/fuzz_targets/inputmap_gamepad.rs`)
//! and the body of the seeded tests below.
//!
//! **The stream lags.** The model produces records into a queue (the
//! consumer's ring) and `inputd`'s loop drains it: by default after every
//! operation, but a script can switch the lag on (op 11) so that records pile
//! up, and read the kernel's table (op 6) *ahead* of them. The table is then
//! held with the sequence number the kernel returned with it and applied at its
//! place in the stream, as in the service. (An earlier model read the table at
//! the same point it was applied, which is why it could not see the table
//! running ahead of the records still queued.)
//!
//! **Invariants.** Replaying the events a client receives reproduces the
//! engine's state exactly (so no event repeats a state, and an unplug, a
//! replaced source or a `Dropped` leaves nothing held or deflected); a device
//! with no slot has nothing held in the client's view; when a table is applied
//! the classifier agrees with it about who is a pad; an engine slot always
//! belongs to a classified pad; a known pad's records never reach the cursor;
//! **as long as no `SOURCE` record of the device was lost (or forged) since the
//! last table applied, a record goes to the engine that owns its device at the
//! time it was published, however far the table ran ahead**; and no record,
//! however hostile, panics.

use std::vec::Vec;

use crate::gamepad::pads::{Notice, Pads};
use crate::gamepad::route::{Route, SourceEntry, GAMEPAD_CLASS};
use crate::gamepad::{code as button, raw, usage, PadOut, RawPad, AXES, MAX_PADS};
use crate::pointer::{Pointer, RawPointer};

/// Device ids a script uses: few, so that ids are reused across classes.
const DEVICES: usize = 8;

struct Script<'a> {
    data: &'a [u8],
    at: usize,
}

impl Script<'_> {
    fn done(&self) -> bool {
        self.at >= self.data.len()
    }

    fn byte(&mut self) -> u8 {
        let byte = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.byte(), self.byte()])
    }

    fn i32(&mut self) -> i32 {
        i32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }
}

/// What the client believes, built only from the events it was sent.
#[derive(Clone, Copy, Default)]
struct Belief {
    buttons: u32,
    dpad: u8,
    axes: [i16; AXES],
}

/// One thing in the consumer's ring.
#[derive(Clone, Copy)]
enum Queued {
    /// A record, and the class its device had in the kernel when it was
    /// published (`None`: no source).
    Record(RawPad, Option<u8>),
    /// A loss marker at this `seq`.
    Dropped(u64),
}

struct World {
    pads: Pads,
    cursor: Pointer,
    seq: u64,
    out: Vec<PadOut>,
    notices: Vec<Notice>,
    /// The kernel's source table: class and generation per device id.
    table: [Option<(u8, u16)>; DEVICES],
    generation: u16,
    belief: [Belief; DEVICES],
    /// Per device: events of one pad never go back in sequence.
    last_seq: [u64; DEVICES],
    /// The consumer's ring, and whether the loop is letting it fill.
    queue: Vec<Queued>,
    lag: bool,
    /// The newest table read: class per device, and its cut.
    read: Option<([Option<u8>; DEVICES], u64)>,
    /// The cut of the table last checked against the classifier.
    checked: u64,
    /// Per device: `seq` of the newest `SOURCE` that was lost or forged (0:
    /// none). While a table with a cut at or below it is the newest applied,
    /// the classifier may be stale for the device.
    taint: [u64; DEVICES],
}

impl World {
    fn new() -> World {
        World {
            pads: Pads::new(),
            cursor: Pointer::new(640, 480),
            seq: 0,
            out: Vec::new(),
            notices: Vec::new(),
            table: [None; DEVICES],
            generation: 0,
            belief: [Belief::default(); DEVICES],
            last_seq: [0; DEVICES],
            queue: Vec::new(),
            lag: false,
            read: None,
            checked: 0,
            taint: [0; DEVICES],
        }
    }

    fn record(&mut self, device: u8, kind: u8, code: u16, value: i32) -> RawPad {
        self.seq += 1;
        RawPad {
            seq: self.seq,
            ts_ns: self.seq * 10,
            device,
            kind,
            code,
            value,
        }
    }

    fn truth(&self, device: u8) -> Option<u8> {
        self.table[usize::from(device)].map(|(class, _)| class)
    }

    /// Publish one bus record; the loop drains unless the script lags it.
    fn bus(&mut self, device: u8, kind: u8, code: u16, value: i32) {
        let record = self.record(device, kind, code, value);
        self.queue.push(Queued::Record(record, self.truth(device)));
        self.after_produce();
    }

    /// Publish a `SOURCE` record; `faithful` when it says what the kernel did.
    fn source(&mut self, device: u8, class: u16, registered: i32, faithful: bool) {
        let record = self.record(device, raw::SOURCE, class, registered);
        if !faithful {
            self.taint[usize::from(device)] = record.seq;
        }
        self.queue.push(Queued::Record(record, self.truth(device)));
        self.after_produce();
    }

    fn after_produce(&mut self) {
        if !self.lag {
            self.drain();
        }
    }

    /// A record or marker was lost with no `SOURCE`: the kernel's table moved
    /// on, the stream did not say so.
    fn lose_source(&mut self, device: u8) {
        self.seq += 1;
        self.taint[usize::from(device)] = self.seq;
    }

    /// Read the kernel's table now and hold it, as the service does.
    fn read_table(&mut self) {
        let mut classes = [None; DEVICES];
        let entries: Vec<SourceEntry> = self
            .table
            .iter()
            .enumerate()
            .filter_map(|(device, slot)| {
                slot.map(|(class, generation)| {
                    classes[device] = Some(class);
                    SourceEntry {
                        device: device as u8,
                        class,
                        generation,
                    }
                })
            })
            .collect();
        let cut = self.seq + 1;
        self.pads.table(&entries, cut);
        self.read = Some((classes, cut));
    }

    /// The bootstrap read (the service's start): read, and apply at once.
    /// With records still queued that is the model of an unfixable window, so
    /// only the empty queue is applied.
    fn bootstrap(&mut self) {
        self.read_table();
        if self.queue.is_empty() {
            self.pads.settle(&mut self.out, &mut self.notices);
            self.check_applied();
            self.replay();
        }
    }

    /// A marker for lost records: it is in the ring like any other.
    fn dropped(&mut self) {
        self.seq += 1;
        self.queue.push(Queued::Dropped(self.seq));
        self.after_produce();
    }

    /// The loop's pass over the ring: every queued item in order, then the
    /// end of the drain.
    fn drain(&mut self) {
        for item in core::mem::take(&mut self.queue) {
            match item {
                Queued::Record(record, truth) => self.handle(record, truth),
                Queued::Dropped(seq) => self.handle_dropped(seq),
            }
        }
        self.pads.settle(&mut self.out, &mut self.notices);
        self.check_applied();
        self.flush();
    }

    fn reach(&mut self, seq: u64, ts_ns: u64) {
        self.pads
            .reach(seq, ts_ns, &mut self.out, &mut self.notices);
        self.check_applied();
    }

    /// When a table was applied: the classifier agrees with it.
    fn check_applied(&mut self) {
        let applied = self.pads.applied_table();
        if applied == self.checked {
            return;
        }
        self.checked = applied;
        let (classes, cut) = self.read.expect("a table was applied without a read");
        assert_eq!(cut, applied, "an older table was applied");
        for (device, class) in classes.iter().enumerate() {
            assert_eq!(
                self.pads.classifier().is_pad(device as u8),
                *class == Some(GAMEPAD_CLASS),
                "classifier and table disagree on {device}"
            );
        }
    }

    fn handle(&mut self, record: RawPad, truth: Option<u8>) {
        self.reach(record.seq, record.ts_ns);
        let device = record.device;
        if record.kind == raw::SOURCE {
            self.pads.source(record, &mut self.out, &mut self.notices);
            self.replay();
            return;
        }
        let was_pad = self.pads.classifier().is_pad(device);
        let route = self.pads.route(device, record.kind);
        let taint = self.taint[usize::from(device)];
        if taint == 0 || taint < self.pads.applied_table() {
            // The stream told the whole story of this device: a record is
            // its source's, whatever the table has said since.
            let pad = truth == Some(GAMEPAD_CLASS);
            assert!(
                !(pad && route == Route::Pointer),
                "a pad's record reached the cursor: {record:?}"
            );
            assert!(
                !(!pad && route == Route::Pad),
                "a record of {truth:?} went to the pads: {record:?}"
            );
        }
        match route {
            Route::Pad => {
                assert!(was_pad, "routed to the pads without being one");
                self.pads.feed(record, &mut self.out);
            }
            Route::Pointer => {
                assert!(!was_pad, "a pad's record reached the cursor: {record:?}");
                self.cursor.apply(
                    RawPointer {
                        seq: record.seq,
                        ts_ns: record.ts_ns,
                        device,
                        kind: record.kind,
                        code: record.code,
                        value: record.value,
                    },
                    &mut Vec::new(),
                );
            }
            Route::Drop => {}
        }
        self.replay();
    }

    fn handle_dropped(&mut self, seq: u64) {
        self.reach(seq, seq * 10);
        self.pads.resync(seq * 10, seq, &mut self.out);
        self.replay();
        self.read_table();
        self.flush();
        for belief in &self.belief {
            assert_eq!(
                belief.buttons | u32::from(belief.dpad),
                0,
                "held after Dropped"
            );
            assert_eq!(belief.axes, [0; AXES], "deflected after Dropped");
        }
    }

    fn flush(&mut self) {
        self.pads.flush(&mut self.out);
        self.replay();
        // After a flush the client's whole view matches the engine's.
        for device in 0..DEVICES {
            let belief = self.belief[device];
            match self.pads.engine().pad(device as u8) {
                Some(pad) => {
                    assert_eq!(belief.axes, pad.axes, "axes of {device}");
                    assert_eq!(belief.buttons, pad.buttons, "buttons of {device}");
                    assert_eq!(belief.dpad, pad.dpad(), "d-pad of {device}");
                }
                None => {
                    assert_eq!(belief.buttons | u32::from(belief.dpad), 0, "{device} stuck");
                    assert_eq!(belief.axes, [0; AXES], "{device} deflected");
                }
            }
        }
    }

    /// Replay what was emitted into the client's view and check each event.
    fn replay(&mut self) {
        for event in self.out.drain(..) {
            assert!(usize::from(event.device()) < DEVICES);
            let last = &mut self.last_seq[usize::from(event.device())];
            assert!(event.seq() >= *last, "seq went back: {event:?}");
            *last = event.seq();
            let belief = &mut self.belief[usize::from(event.device())];
            match event {
                PadOut::Button { code, pressed, .. } => {
                    let changed = match code {
                        1..=32 => edge(&mut belief.buttons, 1u32 << (code - 1), pressed),
                        button::DPAD_UP..=button::DPAD_RIGHT => {
                            let bit = 1u32 << (code - button::DPAD_UP);
                            let mut wide = u32::from(belief.dpad);
                            let changed = edge(&mut wide, bit, pressed);
                            belief.dpad = wide as u8;
                            changed
                        }
                        _ => panic!("button code {code}"),
                    };
                    assert!(changed, "repeated edge {event:?}");
                }
                PadOut::Axis { code, value, .. } => {
                    assert!((usage::X..=usage::X + 7).contains(&code), "axis {code:#x}");
                    let axis = &mut belief.axes[usize::from(code - usage::X)];
                    assert_ne!(*axis, value, "axis event repeats its value: {event:?}");
                    *axis = value;
                }
            }
        }
        assert!(self.pads.engine().count() <= MAX_PADS);
        for device in 0..DEVICES {
            if self.pads.engine().pad(device as u8).is_some() {
                assert!(
                    self.pads.classifier().is_pad(device as u8),
                    "slot without a pad"
                );
            }
        }
        self.notices.clear();
    }
}

/// Set or clear `bit`; whether that changed anything.
fn edge(held: &mut u32, bit: u32, pressed: bool) -> bool {
    let was = *held & bit != 0;
    if pressed {
        *held |= bit;
    } else {
        *held &= !bit;
    }
    was != pressed
}

/// Run one script. Panics on any violated invariant.
pub fn run(data: &[u8]) {
    let mut script = Script { data, at: 0 };
    let mut world = World::new();
    while !script.done() {
        let op = script.byte();
        let device = script.byte() & (DEVICES as u8 - 1);
        match (op & 0x7F) % 13 {
            // A button edge: usages 0..=35 so both bounds are crossed.
            0 | 1 => {
                let usage = u16::from(script.byte() % 36);
                world.bus(device, raw::BUTTON, usage, i32::from(script.byte() % 3));
            }
            // An axis, the hat, or something near them.
            2 | 3 => {
                let code = 0x2F + u16::from(script.byte() % 12);
                let value = match code {
                    0x39 => i32::from(script.byte() % 11) - 1,
                    _ if op & 0x80 != 0 => script.i32(),
                    _ => i32::from(script.u16() as i16),
                };
                world.bus(device, raw::AXIS, code, value);
            }
            // The kernel registers a source; its record may be lost.
            4 => {
                let class = script.byte() % 5;
                let lost = op & 0x80 != 0;
                if class == 0 {
                    // Not a class: a hostile record at most.
                    world.source(device, 0, 1, false);
                    continue;
                }
                world.generation = world.generation.wrapping_add(1);
                world.table[usize::from(device)] = Some((class, world.generation));
                if lost {
                    world.lose_source(device);
                } else {
                    world.source(device, u16::from(class), 1, true);
                }
            }
            // A source closes; its record may be lost.
            5 => {
                world.table[usize::from(device)] = None;
                if op & 0x80 == 0 {
                    // A class of 0 is a malformed record: ignored, so the
                    // classifier keeps a device the kernel freed.
                    let class = script.byte() % 5;
                    world.source(device, u16::from(class), 0, class != 0);
                } else {
                    world.lose_source(device);
                }
            }
            // The kernel's table, read now (ahead of anything still queued).
            6 => world.bootstrap(),
            7 => world.flush(),
            8 => world.dropped(),
            // A pointer record of any device.
            9 => {
                let kind = 2 + script.byte() % 4;
                world.bus(device, kind, u16::from(script.byte() % 3), script.i32());
            }
            // Lag: let the ring fill, or let the loop catch up.
            11 => {
                world.lag = !world.lag;
                if !world.lag {
                    world.drain();
                }
            }
            12 => world.drain(),
            // Anything at all.
            _ => {
                let (kind, code, value) = (script.byte(), script.u16(), script.i32());
                if kind == raw::SOURCE {
                    // A forgery: whatever it does to the classifier, the
                    // kernel's table did not change.
                    world.source(device, code, value, false);
                } else {
                    world.bus(device, kind, code, value);
                }
            }
        }
    }
    world.lag = false;
    world.drain();
}

#[cfg(test)]
mod seeded;
