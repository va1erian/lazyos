//! The gamepad engine: deadzone, hat to d-pad, coalescing, slots, loss
//! recovery and malformed records. Routing is `gamepad_route.rs`, the shared
//! page `gamepad_page.rs`.

use alloc::vec::Vec;

use crate::gamepad::{
    code, deadzone, dpad, dpad_of, raw, usage, Gamepad, PadOut, PadState, RawPad, DEADZONE,
    HAT_CENTRED, MAX_PADS,
};

pub(super) const PAD: u8 = 3;
pub(super) const OTHER_PAD: u8 = 6;

/// A bare engine and the records it was fed.
pub(super) struct Rig {
    pub pad: Gamepad,
    pub seq: u64,
    pub out: Vec<PadOut>,
}

impl Rig {
    pub fn new() -> Rig {
        let mut pad = Gamepad::new();
        assert!(pad.attach(PAD));
        Rig {
            pad,
            seq: 0,
            out: Vec::new(),
        }
    }

    pub fn record(&mut self, device: u8, kind: u8, code: u16, value: i32) -> bool {
        self.seq += 1;
        let record = RawPad {
            seq: self.seq,
            ts_ns: self.seq * 1000,
            device,
            kind,
            code,
            value,
        };
        self.pad.apply(record, &mut self.out)
    }

    pub fn button(&mut self, usage: u16, pressed: bool) {
        assert!(self.record(PAD, raw::BUTTON, usage, i32::from(pressed)));
    }

    pub fn axis(&mut self, code: u16, value: i32) {
        assert!(self.record(PAD, raw::AXIS, code, value));
    }

    pub fn hat(&mut self, value: i32) {
        assert!(self.record(PAD, raw::AXIS, usage::HAT, value));
    }

    /// Flush and take everything emitted.
    pub fn take(&mut self) -> Vec<PadOut> {
        self.pad.flush(&mut self.out);
        core::mem::take(&mut self.out)
    }
}

fn presses(out: &[PadOut]) -> Vec<(u16, bool)> {
    out.iter()
        .filter_map(|event| match *event {
            PadOut::Button { code, pressed, .. } => Some((code, pressed)),
            PadOut::Axis { .. } => None,
        })
        .collect()
}

fn axes(out: &[PadOut]) -> Vec<(u16, i16)> {
    out.iter()
        .filter_map(|event| match *event {
            PadOut::Axis { code, value, .. } => Some((code, value)),
            PadOut::Button { .. } => None,
        })
        .collect()
}

#[test]
fn deadzone_edges_and_full_range() {
    assert_eq!(deadzone(0), 0);
    assert_eq!(deadzone(DEADZONE as i16), 0);
    assert_eq!(deadzone(-(DEADZONE as i16)), 0);
    assert_eq!(deadzone(DEADZONE as i16 + 1), 1);
    assert_eq!(deadzone(-(DEADZONE as i16) - 1), -1);
    assert_eq!(deadzone(i16::MAX), i16::MAX);
    assert_eq!(deadzone(i16::MIN), i16::MIN);
}

#[test]
fn deadzone_is_monotone_and_nearly_symmetric() {
    let mut previous = i16::MIN;
    for raw in i16::MIN..=i16::MAX {
        let value = deadzone(raw);
        assert!(value >= previous, "not monotone at {raw}");
        previous = value;
        if raw != i16::MIN {
            // Both sides map their own full scale, so mirrored inputs differ
            // by at most the one count the asymmetric scales cost.
            let mirrored = deadzone(-raw);
            assert!(
                (i32::from(value) + i32::from(mirrored)).abs() <= 1,
                "at {raw}"
            );
        }
    }
}

#[test]
fn axis_event_is_deadzoned_and_rescaled() {
    let mut rig = Rig::new();
    rig.axis(usage::X, 3000);
    assert!(rig.take().is_empty(), "inside the deadzone");
    rig.axis(usage::X, 32767);
    rig.axis(usage::X + 1, -32768);
    rig.axis(usage::X + 2, 20000);
    let out = rig.take();
    assert_eq!(
        axes(&out),
        [
            (usage::X, 32767),
            (usage::X + 1, -32768),
            (usage::X + 2, deadzone(20000))
        ]
    );
    assert_eq!(rig.pad.pad(PAD).unwrap().axes[0], 32767);
}

#[test]
fn every_plain_axis_is_reachable_and_the_hat_is_not_one() {
    let mut rig = Rig::new();
    for code in usage::X..=usage::DIAL {
        rig.axis(code, 30000);
    }
    let out = rig.take();
    assert_eq!(out.len(), 8);
    assert!(out
        .iter()
        .all(|e| matches!(e, PadOut::Axis { value, .. } if *value > 0)));
    // The hat never shows up as an axis event.
    rig.hat(2);
    assert!(axes(&rig.take()).is_empty());
}

#[test]
fn axes_coalesce_until_flush() {
    let mut rig = Rig::new();
    for value in [10_000, 20_000, 30_000] {
        rig.axis(usage::X, value);
    }
    assert!(rig.out.is_empty(), "an axis waits for the flush");
    let out = rig.take();
    assert_eq!(axes(&out), [(usage::X, deadzone(30_000))]);
    // Out and back inside one drain: the session never saw it move.
    rig.axis(usage::X, 0);
    rig.axis(usage::X, 30_000);
    assert!(rig.take().is_empty());
    // And back to rest is an event.
    rig.axis(usage::X, 100);
    assert_eq!(axes(&rig.take()), [(usage::X, 0)]);
}

#[test]
fn axis_event_carries_the_newest_stamp() {
    let mut rig = Rig::new();
    rig.axis(usage::X + 1, 30000);
    rig.axis(usage::X + 1, 31000);
    let out = rig.take();
    let [PadOut::Axis { seq, ts_ns, .. }] = out[..] else {
        panic!("{out:?}")
    };
    assert_eq!((seq, ts_ns), (2, 2000));
    assert_eq!(rig.pad.last_seq(), 2);
}

#[test]
fn button_edges_are_immediate_and_ordered() {
    let mut rig = Rig::new();
    rig.button(1, true);
    rig.button(32, true);
    rig.button(2, true);
    rig.button(1, false);
    assert_eq!(
        presses(&rig.out),
        [(1, true), (32, true), (2, true), (1, false)]
    );
    assert_eq!(rig.pad.pad(PAD).unwrap().buttons, 1 << 31 | 1 << 1);
    assert!(rig.pad.pad(PAD).unwrap().is_down(32));
    assert!(!rig.pad.pad(PAD).unwrap().is_down(1));
    assert!(!rig.pad.pad(PAD).unwrap().is_down(0));
    assert!(!rig.pad.pad(PAD).unwrap().is_down(33));
}

#[test]
fn a_repeated_edge_is_accepted_but_silent() {
    let mut rig = Rig::new();
    rig.button(4, true);
    rig.button(4, true);
    rig.button(5, false);
    assert_eq!(presses(&rig.out), [(4, true)]);
    assert_eq!(rig.pad.rejected(), 0);
}

#[test]
fn hat_becomes_dpad_edges() {
    let mut rig = Rig::new();
    rig.hat(0);
    assert_eq!(presses(&rig.out), [(code::DPAD_UP, true)]);
    rig.out.clear();
    // Up to right: the release comes before the press.
    rig.hat(2);
    assert_eq!(
        presses(&rig.out),
        [(code::DPAD_UP, false), (code::DPAD_RIGHT, true)]
    );
    rig.out.clear();
    rig.hat(i32::from(HAT_CENTRED));
    assert_eq!(presses(&rig.out), [(code::DPAD_RIGHT, false)]);
    assert_eq!(rig.pad.pad(PAD).unwrap().hat, HAT_CENTRED);
    assert_eq!(rig.pad.pad(PAD).unwrap().dpad(), 0);
}

#[test]
fn diagonals_press_two_directions() {
    let cases = [
        (1, dpad::UP | dpad::RIGHT, [code::DPAD_UP, code::DPAD_RIGHT]),
        (
            3,
            dpad::DOWN | dpad::RIGHT,
            [code::DPAD_DOWN, code::DPAD_RIGHT],
        ),
        (
            5,
            dpad::DOWN | dpad::LEFT,
            [code::DPAD_DOWN, code::DPAD_LEFT],
        ),
        (7, dpad::UP | dpad::LEFT, [code::DPAD_UP, code::DPAD_LEFT]),
    ];
    for (hat, bits, codes) in cases {
        let mut rig = Rig::new();
        rig.hat(hat);
        assert_eq!(presses(&rig.out), [(codes[0], true), (codes[1], true)]);
        assert_eq!(rig.pad.pad(PAD).unwrap().dpad(), bits);
        rig.out.clear();
        // Sliding round to the next cardinal keeps one direction, releases one.
        rig.hat((hat + 1) % 8);
        assert_eq!(rig.out.len(), 1, "hat {hat}");
        assert!(matches!(rig.out[0], PadOut::Button { pressed: false, .. }));
    }
}

#[test]
fn the_same_hat_twice_is_silent() {
    let mut rig = Rig::new();
    rig.hat(4);
    rig.out.clear();
    rig.hat(4);
    assert!(rig.out.is_empty());
}

#[test]
fn dpad_bits_cover_the_hat() {
    for hat in 0..=7 {
        assert_ne!(dpad_of(hat), 0);
        assert_eq!(
            PadState {
                hat,
                ..PadState::new(1)
            }
            .dpad(),
            dpad_of(hat)
        );
    }
    assert_eq!(dpad_of(HAT_CENTRED), 0);
    assert_eq!(dpad_of(200), 0);
    assert_eq!(PadState::default().hat, HAT_CENTRED);
}

#[test]
fn two_pads_are_independent() {
    let mut rig = Rig::new();
    assert!(rig.pad.attach(OTHER_PAD));
    rig.button(1, true);
    assert!(rig.record(OTHER_PAD, raw::BUTTON, 2, 1));
    assert!(rig.record(OTHER_PAD, raw::AXIS, usage::X, 30000));
    let out = rig.take();
    assert!(out.iter().any(|e| e.device() == PAD));
    assert!(out.iter().any(|e| e.device() == OTHER_PAD));
    assert_eq!(rig.pad.pad(PAD).unwrap().buttons, 1);
    assert_eq!(rig.pad.pad(OTHER_PAD).unwrap().buttons, 2);
    assert_eq!(rig.pad.pad(PAD).unwrap().axes[0], 0);
    assert_eq!(rig.pad.count(), 2);
    let devices: Vec<u8> = rig.pad.pads().map(|pad| pad.device).collect();
    assert_eq!(devices, [PAD, OTHER_PAD]);
}

#[test]
fn a_fifth_pad_is_refused_and_its_records_are_counted() {
    let mut rig = Rig::new();
    for device in 10..10 + MAX_PADS as u8 - 1 {
        assert!(rig.pad.attach(device));
    }
    assert_eq!(rig.pad.count(), MAX_PADS);
    assert!(!rig.pad.attach(99));
    assert!(rig.pad.attach(PAD), "an attached pad stays attached");
    assert!(!rig.record(99, raw::BUTTON, 1, 1));
    assert_eq!(rig.pad.rejected(), 1);
    assert!(rig.out.is_empty());
    // A slot freed by an unplug is reusable.
    rig.pad.detach(10, 5, 5, &mut rig.out);
    assert!(rig.pad.attach(99));
    assert!(rig.record(99, raw::BUTTON, 1, 1));
}

#[test]
fn resync_releases_everything_and_keeps_the_pads() {
    let mut rig = Rig::new();
    assert!(rig.pad.attach(OTHER_PAD));
    rig.button(1, true);
    rig.button(32, true);
    rig.hat(1);
    rig.axis(usage::X, 30000);
    assert!(rig.record(OTHER_PAD, raw::BUTTON, 5, 1));
    let _ = rig.take();
    rig.pad.resync(77, 99, &mut rig.out);
    let out = core::mem::take(&mut rig.out);
    let releases = presses(&out);
    for expected in [
        (1, false),
        (32, false),
        (code::DPAD_UP, false),
        (code::DPAD_RIGHT, false),
        (5, false),
    ] {
        assert!(releases.contains(&expected), "{expected:?} in {releases:?}");
    }
    assert_eq!(axes(&out), [(usage::X, 0)]);
    assert!(out.iter().all(|e| e.seq() == 99));
    for pad in rig.pad.pads() {
        assert_eq!(*pad, PadState::new(pad.device));
    }
    assert_eq!(rig.pad.count(), 2);
    assert_eq!(rig.pad.last_seq(), 99);
    // Nothing more to say the second time.
    rig.pad.resync(78, 100, &mut rig.out);
    assert!(rig.out.is_empty());
}

#[test]
fn an_unsent_deflection_is_centred_without_an_event() {
    let mut rig = Rig::new();
    rig.axis(usage::X, 30000);
    rig.pad.resync(1, 50, &mut rig.out);
    assert!(rig.take().is_empty(), "the session never saw it move");
    assert_eq!(rig.pad.pad(PAD).unwrap().axes, [0; 8]);
}

#[test]
fn detach_releases_centres_and_frees_the_slot() {
    let mut rig = Rig::new();
    rig.button(3, true);
    rig.hat(6);
    rig.axis(usage::DIAL, -30000);
    let _ = rig.take();
    rig.pad.detach(PAD, 7, 8, &mut rig.out);
    let out = core::mem::take(&mut rig.out);
    assert_eq!(presses(&out), [(3, false), (code::DPAD_LEFT, false)]);
    assert_eq!(axes(&out), [(usage::DIAL, 0)]);
    assert!(rig.pad.pad(PAD).is_none());
    assert_eq!(rig.pad.count(), 0);
    // Gone: its records are refused.
    assert!(!rig.record(PAD, raw::BUTTON, 1, 1));
    // Detaching again, or a stranger, does nothing.
    rig.pad.detach(PAD, 9, 9, &mut rig.out);
    assert!(rig.out.is_empty());
}

#[test]
fn malformed_records_change_nothing_but_the_counter() {
    let mut rig = Rig::new();
    rig.button(1, true);
    rig.out.clear();
    let before = *rig.pad.pad(PAD).unwrap();
    let bad: [(u8, u16, i32); 16] = [
        (raw::BUTTON, 0, 1),
        (raw::BUTTON, 33, 1),
        (raw::BUTTON, 0xFFFF, 1),
        (raw::BUTTON, 1, 2),
        (raw::BUTTON, 1, -1),
        (raw::AXIS, 0x2F, 100),
        (raw::AXIS, 0x38, 100),
        (raw::AXIS, 0x3A, 100),
        (raw::AXIS, 0, 100),
        (raw::AXIS, usage::X, 40_000),
        (raw::AXIS, usage::X, -40_000),
        (raw::AXIS, usage::X, i32::MIN),
        (raw::AXIS, usage::HAT, 9),
        (raw::AXIS, usage::HAT, -1),
        (raw::DEVICE, 0x2dc8, 1),
        (200, 1, 1),
    ];
    for (n, (kind, code, value)) in bad.into_iter().enumerate() {
        assert!(!rig.record(PAD, kind, code, value), "{kind} {code} {value}");
        assert_eq!(rig.pad.rejected(), n as u64 + 1);
    }
    assert!(rig.take().is_empty());
    assert_eq!(*rig.pad.pad(PAD).unwrap(), before);
}

#[test]
fn the_extremes_of_good_records_are_accepted() {
    let mut rig = Rig::new();
    rig.axis(usage::X, i32::from(i16::MAX));
    rig.axis(usage::X + 1, i32::from(i16::MIN));
    rig.hat(7);
    rig.button(1, true);
    rig.button(32, true);
    assert_eq!(rig.pad.rejected(), 0);
}
