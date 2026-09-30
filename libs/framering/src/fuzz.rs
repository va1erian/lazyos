//! Byte-script fuzzing of the ring against a reference model.
//!
//! [`run`] reads its input as a script of operations on one ring: push a
//! frame, pop, arm, take the notify, query, scribble on shared memory as a
//! hostile peer would, or start a fresh ring. The same function is the
//! libFuzzer target (`fuzz/fuzz_targets/framering.rs`) and the body of the seeded
//! tests below, so a crash found by one replays under the other.
//!
//! **Invariants.** While nothing has scribbled the ring ("clean") every result
//! must match the model exactly: same order, same bytes, `Full` exactly when
//! all slots are used, notify exactly when armed. After a scribble the ring is
//! "tainted", and the peer may legitimately make it deliver garbage, so only
//! safety is checked: nothing panics, a delivered length never exceeds
//! [`MAX_FRAME`], a poisoned endpoint stays poisoned, and the guard pages on
//! both sides of the region are never touched. Scribbling on the `armed` word
//! is advisory-only and keeps the ring clean.

use std::collections::VecDeque;
use std::vec::Vec;

use crate::{
    off, ring_bytes, Consumer, FrameBuf, InitError, PopError, Producer, PushError, Ring,
    HEADER_BYTES, MAX_FRAME, SLOT_BYTES,
};

const PAGE: usize = 4096;
const GUARD_BYTE: u8 = 0xA5;

/// Ring memory between two guard pages that must stay untouched. Allocated
/// zeroed straight from the allocator (a fuzz run makes one per script, and
/// cloning pages would dominate the run time).
pub struct Mem {
    block: *mut u8,
    layout: std::alloc::Layout,
    len: usize,
}

impl Mem {
    pub fn new(slots: u32) -> Mem {
        Mem::with_len(ring_bytes(slots))
    }

    /// Zeroed, page-aligned memory of `len` bytes between two guard pages (for
    /// tests that put more than one ring, or a device's queues, in one block).
    pub fn with_len(len: usize) -> Mem {
        let layout = std::alloc::Layout::from_size_align(len + 2 * PAGE, PAGE).expect("layout");
        // SAFETY: the layout has a non-zero size.
        let block = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!block.is_null(), "out of memory");
        let mem = Mem { block, layout, len };
        // SAFETY: both guard ranges are inside the allocation.
        unsafe {
            core::ptr::write_bytes(mem.block, GUARD_BYTE, PAGE);
            core::ptr::write_bytes(mem.block.add(PAGE + len), GUARD_BYTE, PAGE);
        }
        mem
    }

    pub fn base(&mut self) -> *mut u8 {
        // SAFETY: the ring starts one guard page into the allocation.
        unsafe { self.block.add(PAGE) }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The ring bytes, for scribbling and inspection.
    pub fn bytes(&mut self) -> &mut [u8] {
        let len = self.len;
        // SAFETY: the ring region is `len` initialized bytes inside the block.
        unsafe { core::slice::from_raw_parts_mut(self.base(), len) }
    }

    /// Panic if either guard page was written.
    pub fn assert_guards(&self) {
        // SAFETY: both guard ranges are inside the allocation.
        let (before, after) = unsafe {
            (
                core::slice::from_raw_parts(self.block, PAGE),
                core::slice::from_raw_parts(self.block.add(PAGE + self.len), PAGE),
            )
        };
        assert!(
            before.iter().all(|b| *b == GUARD_BYTE),
            "the page before the ring was written"
        );
        assert!(
            after.iter().all(|b| *b == GUARD_BYTE),
            "the page after the ring was written"
        );
    }
}

impl Drop for Mem {
    fn drop(&mut self) {
        // SAFETY: `block` came from `alloc_zeroed` with this layout.
        unsafe { std::alloc::dealloc(self.block, self.layout) };
    }
}

/// Reads script bytes; past the end it yields zeros and reports exhaustion.
struct Script<'a> {
    data: &'a [u8],
    at: usize,
}

impl Script<'_> {
    fn done(&self) -> bool {
        self.at >= self.data.len()
    }

    fn u8(&mut self) -> u8 {
        let byte = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }

    fn u16(&mut self) -> u16 {
        u16::from(self.u8()) << 8 | u16::from(self.u8())
    }

    fn u32(&mut self) -> u32 {
        u32::from(self.u16()) << 16 | u32::from(self.u16())
    }
}

/// Where a ring's indices start, so the `u32` wrap is reachable.
fn start_index(pick: u8, slots: u32) -> u32 {
    match pick % 6 {
        0 => 0,
        1 => 1,
        2 => u32::MAX,
        3 => u32::MAX - slots + 1,
        4 => u32::MAX - 3,
        _ => 0x8000_0000,
    }
}

fn payload(id: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|j| (id as usize).wrapping_mul(31).wrapping_add(j) as u8)
        .collect()
}

/// One ring, its model and its guard state.
struct Rig {
    mem: Mem,
    producer: Producer,
    consumer: Consumer,
    slots: u32,
    model: VecDeque<Vec<u8>>,
    /// The `armed` word as the model believes it.
    armed: u32,
    tainted: bool,
    pushed: u64,
}

impl Rig {
    fn new(slots: u32, start: u32) -> Rig {
        let mut mem = Mem::new(slots);
        // SAFETY: `mem` outlives the ring (both live in this struct and the
        // pages never move: the Vec is not resized).
        let ring = unsafe { Ring::create(mem.base(), mem.len(), slots) }.expect("valid geometry");
        let (producer, consumer) = ring.endpoints_at(start);
        Rig {
            mem,
            producer,
            consumer,
            slots,
            model: VecDeque::new(),
            armed: 0,
            tainted: false,
            pushed: 0,
        }
    }

    fn push(&mut self, len: usize) {
        self.pushed += 1;
        let frame = payload(self.pushed, len);
        let result = self.producer.push(&frame);
        if self.tainted {
            if self.producer.is_poisoned() && len != 0 && len <= MAX_FRAME {
                assert_eq!(result, Err(PushError::Corrupt), "poison must be sticky");
            }
            return;
        }
        let expected = if len == 0 {
            Err(PushError::Empty)
        } else if len > MAX_FRAME {
            Err(PushError::TooLong)
        } else if self.model.len() as u32 == self.slots {
            Err(PushError::Full)
        } else {
            Ok(())
        };
        assert_eq!(
            result,
            expected,
            "push of {len} bytes with {} queued",
            self.model.len()
        );
        if result.is_ok() {
            self.model.push_back(frame);
        }
    }

    fn pop(&mut self) {
        let mut out: FrameBuf = [0; MAX_FRAME];
        let result = self.consumer.pop(&mut out);
        if self.tainted {
            match result {
                Ok(Some(n)) => assert!(n <= MAX_FRAME),
                Err(PopError::BadLength(n)) => assert!(usize::from(n) > MAX_FRAME),
                Err(PopError::Corrupt) => assert!(self.consumer.is_poisoned()),
                Ok(None) => {}
            }
            if self.consumer.is_poisoned() {
                assert_eq!(
                    self.consumer.pop(&mut out),
                    Err(PopError::Corrupt),
                    "poison must be sticky"
                );
            }
            return;
        }
        match self.model.pop_front() {
            Some(frame) => {
                let n = result
                    .expect("a queued frame pops")
                    .expect("and is delivered");
                assert_eq!(&out[..n], &frame[..], "frame delivered intact and in order");
            }
            None => assert_eq!(result, Ok(None)),
        }
    }

    fn query(&mut self) {
        let pending = self.consumer.pending();
        let free = self.producer.free();
        if self.tainted {
            if let Ok(p) = pending {
                assert!(p <= self.slots);
            }
            if let Ok(f) = free {
                assert!(f <= self.slots);
            }
            return;
        }
        assert_eq!(pending, Ok(self.model.len() as u32));
        assert_eq!(free, Ok(self.slots - self.model.len() as u32));
        assert_eq!(
            self.producer
                .published()
                .wrapping_sub(self.consumer.consumed()),
            self.model.len() as u32
        );
    }

    fn take_notify(&mut self) {
        let got = self.producer.take_notify();
        if !self.tainted {
            assert_eq!(got, self.armed != 0, "notify exactly when armed");
        }
        self.armed = 0;
    }

    fn scribble(&mut self, script: &mut Script) {
        let kind = script.u8() % 7;
        let value = script.u32();
        let slot = usize::from(script.u16()) % self.slots as usize;
        let bytes = self.mem.bytes();
        let word = |bytes: &mut [u8], offset: usize, v: u32| {
            bytes[offset..offset + 4].copy_from_slice(&v.to_le_bytes())
        };
        match kind {
            // The advisory flag: harmless to delivery, so the ring stays clean.
            0 => {
                word(bytes, off::ARMED, value);
                self.armed = value;
                return;
            }
            1 => word(bytes, off::HEAD, value),
            2 => word(bytes, off::TAIL, value),
            3 => word(bytes, HEADER_BYTES + slot * SLOT_BYTES, value & 0xFFFF),
            4 => {
                let at = HEADER_BYTES + slot * SLOT_BYTES + 2 + (value as usize % (MAX_FRAME - 4));
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            5 => word(bytes, off::MAGIC, value),
            _ => {
                let at = HEADER_BYTES + slot * SLOT_BYTES;
                for (j, b) in bytes[at..at + SLOT_BYTES].iter_mut().enumerate() {
                    *b = (value as usize).wrapping_add(j.wrapping_mul(7)) as u8;
                }
            }
        }
        self.tainted = true;
    }
}

/// Interpret `data` as a script; see the module docs for the invariants.
pub fn run(data: &[u8]) {
    let mut script = Script { data, at: 0 };
    let first = script.u8();
    let slots = 16u32 << (first % 5);
    let mut rig = Rig::new(slots, start_index(first / 5, slots));
    let mut steps = 0u32;
    while !script.done() && steps < 100_000 {
        steps += 1;
        match script.u8() {
            0..=109 => {
                let len = usize::from(script.u16()) % (MAX_FRAME + 60);
                rig.push(len);
            }
            110..=189 => rig.pop(),
            190..=199 => {
                rig.consumer.arm();
                rig.armed = 1;
            }
            200..=214 => rig.take_notify(),
            215..=234 => rig.query(),
            235..=244 => rig.scribble(&mut script),
            _ => {
                let pick = script.u8();
                let slots = 16u32 << (pick % 5);
                rig.mem.assert_guards();
                rig = Rig::new(slots, start_index(pick / 5, slots));
            }
        }
        if steps.is_multiple_of(64) {
            rig.mem.assert_guards();
        }
    }
    // Drain: whatever is left must come out (clean) or at least not misbehave.
    for _ in 0..rig.slots + 2 {
        rig.pop();
    }
    rig.mem.assert_guards();
}

/// Fuzz `Ring::attach` with an arbitrary header: it must never panic, and when
/// it accepts a header the header must really describe the ring.
pub fn run_header(data: &[u8]) {
    let mut script = Script { data, at: 0 };
    let slots = [0, 8, 16, 24, 32, 64, 256, 1024, 2048][usize::from(script.u8() % 9)];
    let len_delta = i64::from(script.u8() % 3) - 1;
    let mut mem = Mem::new(if crate::valid_slots(slots) { slots } else { 16 });
    let len = (mem.len() as i64
        + if script.u8().is_multiple_of(4) {
            len_delta
        } else {
            0
        })
    .max(0) as usize;
    // Start from a real header, then let the script overwrite bytes.
    // SAFETY: `mem` is live for the whole function and is the size Mem::new made.
    unsafe {
        Ring::create(
            mem.base(),
            mem.len(),
            if crate::valid_slots(slots) { slots } else { 16 },
        )
    }
    .expect("create");
    while !script.done() {
        let at = usize::from(script.u16()) % 0x100;
        let value = script.u8();
        mem.bytes()[at] = value;
    }
    let base = mem.base();
    // SAFETY: as above; `len` may disagree with the region, which `attach`
    // must reject by length before touching anything.
    let attached = unsafe { Ring::attach(base, len, slots) };
    match attached {
        Ok(ring) => {
            assert!(crate::valid_slots(slots) && len == ring_bytes(slots));
            assert_eq!(ring.slots(), slots);
            let bytes = mem.bytes();
            let word = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
            assert_eq!(word(off::MAGIC), crate::MAGIC);
            assert_eq!(word(off::HEAD), 0);
            assert_eq!(word(off::TAIL), 0);
            assert!(word(off::ARMED) <= 1);
        }
        Err(InitError::BadSlots) => assert!(!crate::valid_slots(slots)),
        Err(InitError::BadLength) => assert!(len != ring_bytes(slots)),
        Err(InitError::BadHeader) => {}
        Err(InitError::Misaligned) => panic!("pages are aligned"),
    }
    mem.assert_guards();
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    /// Replay every checked-in seed (`fuzz/seeds/<target>`) and every saved
    /// crash (`fuzz/regressions/<target>`) through the entry point, so the
    /// corpus stays valid and a fixed bug stays fixed under plain `cargo test`.
    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                let data = std::fs::read(entry.path()).unwrap();
                run(&data);
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    /// A script of `len` ops biased toward clean traffic, with hostile ops
    /// (opcodes 235..) allowed only when `hostile`.
    fn script(rng: &mut Rng, len: usize, hostile: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(len * 4);
        out.push(rng.byte());
        for _ in 0..len {
            let op = if hostile {
                rng.byte()
            } else {
                rng.below(235) as u8
            };
            out.push(op);
            match op {
                0..=109 => {
                    // Mostly sensible sizes, sometimes the boundaries.
                    let len = match rng.below(8) {
                        0 => MAX_FRAME as u16,
                        1 => MAX_FRAME as u16 + 1,
                        2 => 0,
                        3 => 1514,
                        _ => rng.below(1600) as u16,
                    };
                    out.extend_from_slice(&len.to_be_bytes());
                }
                235..=244 => out.extend((0..7).map(|_| rng.byte())),
                245..=255 => out.push(rng.byte()),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn clean_scripts_match_the_model() {
        for_seeds("framering::clean_scripts_match_the_model", |_, rng| {
            let len = rng.range(50, 3000) as usize;
            run(&script(rng, len, false));
        });
    }

    #[test]
    fn hostile_scripts_are_safe() {
        for_seeds("framering::hostile_scripts_are_safe", |_, rng| {
            let len = rng.range(50, 3000) as usize;
            run(&script(rng, len, true));
        });
    }

    #[test]
    fn arbitrary_bytes_are_safe() {
        for_seeds("framering::arbitrary_bytes_are_safe", |_, rng| {
            let len = rng.range(0, 6000) as usize;
            run(&rng.bytes(len));
        });
    }

    #[test]
    fn arbitrary_headers_never_attach_wrongly() {
        for_seeds(
            "framering::arbitrary_headers_never_attach_wrongly",
            |_, rng| {
                let len = rng.range(0, 60) as usize;
                run_header(&rng.bytes(len));
            },
        );
    }

    #[test]
    fn checked_in_corpus_replays() {
        replay("framering", run);
        replay("framering_header", run_header);
    }
}
