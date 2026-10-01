//! Byte-script fuzzing of the pointer state machine against a reference model.
//!
//! [`run`] reads its input as a screen size followed by a script of raw bus
//! records, loss markers, flushes and screen resizes, feeds them to a
//! [`Pointer`], and checks it against a deliberately naive model after every
//! step. The same function is the libFuzzer target
//! (`fuzz/fuzz_targets/inputmap_pointer.rs`) and the body of the seeded tests
//! below, so a crash found by one replays under the other.
//!
//! **Invariants.** The cursor and every output stay inside the bounds; an
//! output's button mask is exactly the model's held set; output sequence
//! numbers never go backwards; malformed records change nothing but the
//! rejection count; and, while every scroll value was small enough not to
//! saturate, the wheel notches delivered plus those still queued equal the
//! notches accepted.

use std::collections::BTreeSet;
use std::vec::Vec;

use crate::pointer::{raw, Pointer, PointerOut, RawPointer, MAX_SIDE};

/// A cursor over the script bytes; reading past the end yields zeros, so
/// every input is a valid script.
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

/// The naive model: what must be held, where the cursor must be.
struct Model {
    width: i64,
    height: i64,
    x: i64,
    y: i64,
    held: BTreeSet<(u16, u8)>,
    wheel_in: i64,
    wheel_out: i64,
    wheel_exact: bool,
    last_seq: u64,
    seq: u64,
}

impl Model {
    fn buttons(&self) -> u32 {
        self.held
            .iter()
            .fold(0, |mask, (code, _)| mask | 1 << (code - 1))
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.width = i64::from(width).clamp(1, i64::from(MAX_SIDE));
        self.height = i64::from(height).clamp(1, i64::from(MAX_SIDE));
        self.x = self.x.clamp(0, self.width - 1);
        self.y = self.y.clamp(0, self.height - 1);
    }

    /// Mirror one record; `false` when it is malformed.
    fn apply(&mut self, record: &RawPointer) -> bool {
        match (record.kind, record.code) {
            (raw::REL_MOTION, 0) => {
                let (dx, dy) = (record.value as i16, (record.value >> 16) as i16);
                self.x = (self.x + i64::from(dx)).clamp(0, self.width - 1);
                self.y = (self.y + i64::from(dy)).clamp(0, self.height - 1);
            }
            (raw::ABS_MOTION, 0) => {
                let (x, y) = (record.value as u16, (record.value as u32 >> 16) as u16);
                self.x = i64::from(x) * (self.width - 1) / 0xFFFF;
                self.y = i64::from(y) * (self.height - 1) / 0xFFFF;
            }
            (raw::SCROLL, 0 | 1) => {
                if record.code == 0 {
                    self.wheel_in += i64::from(record.value);
                }
                self.wheel_exact &= record.value.unsigned_abs() < 1 << 16;
            }
            (raw::BUTTON, 1..=5) if matches!(record.value, 0 | 1) => {
                let key = (record.code, record.device);
                if record.value == 1 {
                    self.held.insert(key);
                } else {
                    self.held.remove(&key);
                }
            }
            _ => return false,
        }
        true
    }

    fn check(&mut self, pointer: &Pointer, outputs: &[PointerOut]) {
        let (w, h) = (self.width as i32, self.height as i32);
        for out in outputs {
            assert!(
                (0..w).contains(&out.x) && (0..h).contains(&out.y),
                "output {out:?} outside {w}x{h}"
            );
            assert!(out.seq >= self.last_seq, "seq went back: {out:?}");
            self.last_seq = out.seq;
            self.wheel_out += i64::from(out.wheel_v);
        }
        if let Some(last) = outputs.last() {
            assert_eq!(last.buttons, self.buttons(), "held buttons");
        }
        assert_eq!(pointer.buttons(), self.buttons(), "held buttons");
        assert_eq!(pointer.position(), (self.x as i32, self.y as i32), "cursor");
        assert_eq!(pointer.bounds(), (w as u32, h as u32), "bounds");
    }
}

/// Run one script. Panics on any violated invariant.
pub fn run(data: &[u8]) {
    let mut script = Script { data, at: 0 };
    let (width, height) = (script.u16(), script.u16());
    let mut pointer = Pointer::new(u32::from(width), u32::from(height));
    let mut model = Model {
        width: 1,
        height: 1,
        x: 0,
        y: 0,
        held: BTreeSet::new(),
        wheel_in: 0,
        wheel_out: 0,
        wheel_exact: true,
        last_seq: 0,
        seq: 0,
    };
    model.resize(width, height);
    model.x = model.width / 2;
    model.y = model.height / 2;
    let mut outputs = Vec::new();
    while !script.done() {
        outputs.clear();
        model.seq += 1;
        let (seq, ts_ns) = (model.seq, model.seq * 10);
        let op = script.byte();
        let device = script.byte() & 7;
        let mut record = RawPointer {
            seq,
            ts_ns,
            device,
            kind: 0,
            code: 0,
            value: 0,
        };
        match op % 8 {
            0 => {
                record.kind = raw::REL_MOTION;
                record.value = i32::from(script.u16()) | i32::from(script.u16()) << 16;
            }
            1 => {
                record.kind = raw::ABS_MOTION;
                record.value = i32::from(script.u16()) | i32::from(script.u16()) << 16;
            }
            2 | 3 => {
                record.kind = raw::BUTTON;
                record.code = u16::from(script.byte() % 7);
                record.value = i32::from(script.byte() % 3);
            }
            4 => {
                record.kind = raw::SCROLL;
                record.code = u16::from(script.byte() % 3);
                record.value = i32::from(script.byte() as i8);
            }
            5 => {
                pointer.resync(ts_ns, seq, &mut outputs);
                model.held.clear();
                model.check(&pointer, &outputs);
                continue;
            }
            6 => {
                let (w, h) = (script.u16(), script.u16());
                pointer.set_bounds(u32::from(w), u32::from(h));
                model.resize(w, h);
                pointer.flush(&mut outputs);
                model.check(&pointer, &outputs);
                continue;
            }
            _ => {
                // Anything at all, as a hostile driver might publish.
                record.kind = script.byte();
                record.code = script.u16();
                record.value = script.i32();
            }
        }
        let rejected = pointer.rejected();
        let well_formed = model.apply(&record);
        pointer.apply(record, &mut outputs);
        assert_eq!(
            pointer.rejected() - rejected,
            u64::from(!well_formed),
            "rejection of {record:?}"
        );
        if op & 0x80 != 0 {
            pointer.flush(&mut outputs);
        }
        model.check(&pointer, &outputs);
    }
    outputs.clear();
    pointer.flush(&mut outputs);
    model.check(&pointer, &outputs);
    if model.wheel_exact {
        assert_eq!(model.wheel_out, model.wheel_in, "wheel notches lost");
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    /// Replay every checked-in seed (`fuzz/seeds/inputmap_pointer`) and saved
    /// crash (`fuzz/regressions/inputmap_pointer`).
    #[test]
    fn checked_in_seeds_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("inputmap_pointer")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for inputmap_pointer");
        }
    }

    /// Random scripts, biased to small screens so the edges are hit often.
    #[test]
    fn random_scripts() {
        for_seeds("inputmap::fuzz::random_scripts", |_, rng: &mut Rng| {
            let mut data = Vec::new();
            for _ in 0..4 {
                data.push(rng.below(8) as u8);
            }
            let len = rng.below(2000) as usize;
            for _ in 0..len {
                data.push(rng.byte());
            }
            run(&data);
        });
    }
}
