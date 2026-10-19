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
//! **Invariants.** Replaying the events a client receives reproduces the
//! engine's state exactly (so no event repeats a state, and an unplug, a
//! replaced source or a `Dropped` leaves nothing held or deflected); a device
//! with no slot has nothing held in the client's view; after a table read the
//! classifier agrees with the table about who is a pad; an engine slot always
//! belongs to a classified pad; a known pad's records never reach the cursor;
//! and no record, however hostile, panics.

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

    /// One bus record, routed the way `inputd` does.
    fn bus(&mut self, device: u8, kind: u8, code: u16, value: i32) {
        let record = self.record(device, kind, code, value);
        let was_pad = self.pads.classifier().is_pad(device);
        let mut sink = Vec::new();
        match self.pads.route(device, kind) {
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
                        kind,
                        code,
                        value,
                    },
                    &mut sink,
                );
            }
            Route::Drop => {}
        }
        self.settle();
    }

    fn source(&mut self, device: u8, class: u16, registered: i32) {
        let record = self.record(device, raw::SOURCE, class, registered);
        self.pads.source(record, &mut self.out, &mut self.notices);
        self.settle();
    }

    fn read_table(&mut self) {
        let entries: Vec<SourceEntry> = self
            .table
            .iter()
            .enumerate()
            .filter_map(|(device, slot)| {
                slot.map(|(class, generation)| SourceEntry {
                    device: device as u8,
                    class,
                    generation,
                })
            })
            .collect();
        self.seq += 1;
        self.pads.table(
            &entries,
            self.seq * 10,
            self.seq,
            &mut self.out,
            &mut self.notices,
        );
        for device in 0..DEVICES {
            let pad = matches!(self.table[device], Some((GAMEPAD_CLASS, _)));
            assert_eq!(
                self.pads.classifier().is_pad(device as u8),
                pad,
                "classifier and table disagree on {device}"
            );
        }
        self.settle();
    }

    fn dropped(&mut self) {
        self.seq += 1;
        self.pads.resync(self.seq * 10, self.seq, &mut self.out);
        self.settle();
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
        self.settle();
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
    fn settle(&mut self) {
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
        match (op & 0x7F) % 11 {
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
                    world.source(device, 0, 1);
                    continue;
                }
                world.generation = world.generation.wrapping_add(1);
                world.table[usize::from(device)] = Some((class, world.generation));
                if !lost {
                    world.source(device, u16::from(class), 1);
                }
            }
            // A source closes; its record may be lost.
            5 => {
                world.table[usize::from(device)] = None;
                if op & 0x80 == 0 {
                    world.source(device, u16::from(script.byte() % 5), 0);
                }
            }
            6 => world.read_table(),
            7 => world.flush(),
            8 => world.dropped(),
            // A pointer record of any device.
            9 => {
                let kind = 2 + script.byte() % 4;
                world.bus(device, kind, u16::from(script.byte() % 3), script.i32());
            }
            // Anything at all.
            _ => {
                let (kind, code, value) = (script.byte(), script.u16(), script.i32());
                if kind == raw::SOURCE {
                    world.source(device, code, value);
                } else {
                    world.bus(device, kind, code, value);
                }
            }
        }
    }
    world.flush();
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    /// Replay every checked-in seed (`fuzz/seeds/inputmap_gamepad`) and saved
    /// crash (`fuzz/regressions/inputmap_gamepad`).
    #[test]
    fn checked_in_seeds_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("inputmap_gamepad")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for inputmap_gamepad");
        }
    }

    /// Random scripts over eight device ids.
    #[test]
    fn random_scripts() {
        for_seeds(
            "inputmap::fuzz_gamepad::random_scripts",
            |_, rng: &mut Rng| {
                let len = rng.below(1500) as usize;
                let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
                run(&data);
            },
        );
    }

    /// Scripts biased to register pads first, so the engine sees traffic.
    #[test]
    fn pad_heavy_scripts() {
        for_seeds("inputmap::fuzz_gamepad::pad_heavy", |_, rng: &mut Rng| {
            let mut data = Vec::new();
            for device in 0..6u8 {
                data.extend([4, device, GAMEPAD_CLASS]);
            }
            let len = rng.below(1500) as usize;
            for _ in 0..len {
                // Mostly buttons, axes and flushes.
                let op = [0u8, 1, 2, 3, 7, 0, 2, 8][rng.below(8) as usize];
                data.push(op | (rng.byte() & 0x80));
                for _ in 0..5 {
                    data.push(rng.byte());
                }
            }
            run(&data);
        });
    }
}
