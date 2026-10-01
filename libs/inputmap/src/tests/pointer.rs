//! The pointer engine: clamping, scaling, multi-device buttons, loss
//! recovery, wheel accumulation, coalescing and malformed records.

use alloc::vec::Vec;

use crate::pointer::{buttons, raw, Pointer, PointerOut, RawPointer};

const MOUSE: u8 = 2;
const OTHER: u8 = 3;

struct Rig {
    pointer: Pointer,
    seq: u64,
    out: Vec<PointerOut>,
}

impl Rig {
    fn new(width: u32, height: u32) -> Rig {
        Rig {
            pointer: Pointer::new(width, height),
            seq: 0,
            out: Vec::new(),
        }
    }

    fn record(&mut self, device: u8, kind: u8, code: u16, value: i32) {
        self.seq += 1;
        let record = RawPointer {
            seq: self.seq,
            ts_ns: self.seq * 1000,
            device,
            kind,
            code,
            value,
        };
        self.pointer.apply(record, &mut self.out);
    }

    fn rel(&mut self, dx: i16, dy: i16) {
        let value = (u32::from(dx as u16) | u32::from(dy as u16) << 16) as i32;
        self.record(MOUSE, raw::REL_MOTION, 0, value);
    }

    fn abs(&mut self, x: u16, y: u16) {
        let value = (u32::from(x) | u32::from(y) << 16) as i32;
        self.record(OTHER, raw::ABS_MOTION, 0, value);
    }

    fn button(&mut self, device: u8, usage: u16, pressed: bool) {
        self.record(device, raw::BUTTON, usage, i32::from(pressed));
    }

    /// Flush and take everything emitted so far.
    fn take(&mut self) -> Vec<PointerOut> {
        self.pointer.flush(&mut self.out);
        core::mem::take(&mut self.out)
    }
}

#[test]
fn starts_centred_and_clamps_at_every_edge() {
    let mut rig = Rig::new(800, 600);
    assert_eq!(rig.pointer.position(), (400, 300));
    rig.rel(-1000, 0);
    assert_eq!(rig.pointer.position(), (0, 300));
    rig.rel(0, -1000);
    assert_eq!(rig.pointer.position(), (0, 0));
    rig.rel(i16::MAX, 0);
    assert_eq!(rig.pointer.position(), (799, 0));
    rig.rel(0, i16::MAX);
    assert_eq!(rig.pointer.position(), (799, 599));
    rig.rel(-9, -9);
    assert_eq!(rig.pointer.position(), (790, 590));
}

#[test]
fn motion_is_coalesced_into_one_output_per_flush() {
    let mut rig = Rig::new(800, 600);
    for _ in 0..50 {
        rig.rel(1, 2);
    }
    let out = rig.take();
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].x, out[0].y, out[0].seq), (450, 400, 50));
    // Nothing new: nothing out.
    assert!(rig.take().is_empty());
}

#[test]
fn motion_swallowed_by_the_clamp_emits_nothing() {
    let mut rig = Rig::new(800, 600);
    rig.rel(-1000, -1000);
    assert_eq!(rig.take().len(), 1);
    rig.rel(-5, -5);
    assert!(rig.take().is_empty(), "no change, no output");
    // A wheel notch at the edge still reports.
    rig.record(MOUSE, raw::SCROLL, 0, 1);
    assert_eq!(rig.take().len(), 1);
    // A burst that ends where it started is not a move either.
    rig.rel(10, 0);
    rig.rel(-10, 0);
    assert!(rig.take().is_empty());
}

#[test]
fn absolute_positions_scale_to_the_screen() {
    let mut rig = Rig::new(1024, 768);
    rig.abs(0, 0);
    assert_eq!(rig.pointer.position(), (0, 0));
    rig.abs(0xFFFF, 0xFFFF);
    assert_eq!(rig.pointer.position(), (1023, 767));
    rig.abs(0x8000, 0x4000);
    assert_eq!(rig.pointer.position(), (511, 191));
    // A resize re-clamps, and absolute input then scales to the new size.
    rig.abs(0xFFFF, 0xFFFF);
    assert!(rig.pointer.set_bounds(640, 480));
    assert_eq!(rig.pointer.position(), (639, 479));
    assert!(!rig.pointer.set_bounds(640, 480));
    rig.abs(0xFFFF, 0);
    assert_eq!(rig.pointer.position(), (639, 0));
}

#[test]
fn degenerate_bounds_are_clamped() {
    let mut rig = Rig::new(0, 0);
    assert_eq!(rig.pointer.bounds(), (1, 1));
    rig.rel(5, 5);
    assert_eq!(rig.pointer.position(), (0, 0));
    rig.pointer.set_bounds(u32::MAX, 1);
    assert_eq!(rig.pointer.bounds(), (crate::pointer::MAX_SIDE, 1));
}

#[test]
fn every_button_edge_is_its_own_output_after_the_motion_before_it() {
    let mut rig = Rig::new(800, 600);
    rig.rel(10, 0);
    rig.button(MOUSE, 1, true);
    rig.rel(5, 0);
    rig.button(MOUSE, 2, true);
    rig.button(MOUSE, 1, false);
    let out = rig.take();
    let shape: Vec<(i32, u32)> = out.iter().map(|o| (o.x, o.buttons)).collect();
    assert_eq!(
        shape,
        [
            (410, buttons::LEFT),
            (415, buttons::LEFT | buttons::RIGHT),
            (415, buttons::RIGHT),
        ]
    );
}

#[test]
fn two_devices_hold_a_button_independently() {
    let mut rig = Rig::new(800, 600);
    rig.button(MOUSE, 1, true);
    rig.button(OTHER, 1, true);
    rig.button(MOUSE, 1, false);
    assert_eq!(rig.pointer.buttons(), buttons::LEFT, "released early");
    // A repeated press or a stray release from one device changes nothing.
    rig.button(MOUSE, 1, false);
    rig.button(OTHER, 1, true);
    assert_eq!(rig.pointer.buttons(), buttons::LEFT);
    rig.button(OTHER, 1, false);
    assert_eq!(rig.pointer.buttons(), 0);
    let masks: Vec<u32> = rig.take().iter().map(|o| o.buttons).collect();
    assert_eq!(masks, [buttons::LEFT, 0]);
    // Back and forward map to the high bits.
    rig.button(MOUSE, 4, true);
    rig.button(MOUSE, 5, true);
    assert_eq!(rig.pointer.buttons(), buttons::BACK | buttons::FORWARD);
}

#[test]
fn a_loss_marker_releases_every_button_but_keeps_the_position() {
    let mut rig = Rig::new(800, 600);
    rig.button(MOUSE, 1, true);
    rig.button(OTHER, 3, true);
    rig.rel(7, 7);
    rig.take();
    let mut out = Vec::new();
    rig.pointer.resync(99, 42, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(
        (out[0].x, out[0].y, out[0].buttons, out[0].seq),
        (407, 307, 0, 42)
    );
    // Nothing held: a second marker is silent.
    out.clear();
    rig.pointer.resync(100, 50, &mut out);
    assert!(out.is_empty());
}

#[test]
fn wheel_notches_accumulate_until_flushed() {
    let mut rig = Rig::new(800, 600);
    for value in [1, 2, -1] {
        rig.record(MOUSE, raw::SCROLL, 0, value);
    }
    rig.record(MOUSE, raw::SCROLL, 1, -4);
    let out = rig.take();
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].wheel_v, out[0].wheel_h), (2, -4));
    rig.rel(1, 0);
    let out = rig.take();
    assert_eq!((out[0].wheel_v, out[0].wheel_h), (0, 0), "wheel reset");
    // A button edge carries the notches queued before it.
    rig.record(MOUSE, raw::SCROLL, 0, 3);
    rig.button(MOUSE, 1, true);
    rig.record(MOUSE, raw::SCROLL, 0, 1);
    let wheels: Vec<i32> = rig.take().iter().map(|o| o.wheel_v).collect();
    assert_eq!(wheels, [3, 1]);
}

#[test]
fn malformed_records_are_dropped_and_counted() {
    let mut rig = Rig::new(800, 600);
    rig.record(MOUSE, raw::BUTTON, 0, 1); // no usage 0
    rig.record(MOUSE, raw::BUTTON, 6, 1); // beyond forward
    rig.record(MOUSE, raw::BUTTON, 1, 2); // not an edge
    rig.record(MOUSE, raw::BUTTON, 1, -1);
    rig.record(MOUSE, raw::SCROLL, 2, 1); // no third axis
    rig.record(MOUSE, raw::REL_MOTION, 1, 5); // motion has code 0
    rig.record(MOUSE, raw::ABS_MOTION, 9, 5);
    rig.record(MOUSE, 1, 4, 1); // a key is not a pointer record
    rig.record(MOUSE, 200, 0, 0);
    assert_eq!(rig.pointer.rejected(), 9);
    assert_eq!(rig.pointer.position(), (400, 300));
    assert_eq!(rig.pointer.buttons(), 0);
    assert!(rig.take().is_empty());
}

#[test]
fn pointer_kinds_match_the_kernel_bus() {
    // The kernel's `bus::kind` values (`kernel/src/input/bus.rs`).
    assert_eq!(
        (raw::REL_MOTION, raw::ABS_MOTION, raw::BUTTON, raw::SCROLL),
        (2, 3, 4, 5)
    );
    assert!(!raw::is_pointer(1) && !raw::is_pointer(7));
    assert!((2..=5).all(raw::is_pointer));
}
