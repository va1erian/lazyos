//! Routing by source class: a pad's records never reach the cursor, a slot a
//! pointer reuses is a pointer again, and a `SOURCE` lost in a `Dropped` gap
//! is recovered from the source table. The rig routes exactly as `inputd`'s
//! loop does, with a real cursor engine on the other side.

use alloc::vec::Vec;

use crate::gamepad::pads::{Notice, Pads};
use crate::gamepad::route::{Route, SourceEntry, GAMEPAD_CLASS};
use crate::gamepad::{code, raw, usage, PadOut, RawPad, MAX_PADS};
use crate::pointer::{raw as pointer_raw, Pointer, PointerOut, RawPointer};

const KEYBOARD: u8 = 1;
const POINTER: u8 = 2;
const TABLET: u8 = 3;

struct Rig {
    pads: Pads,
    cursor: Pointer,
    seq: u64,
    pad_out: Vec<PadOut>,
    cursor_out: Vec<PointerOut>,
    notices: Vec<Notice>,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            pads: Pads::new(),
            cursor: Pointer::new(800, 600),
            seq: 0,
            pad_out: Vec::new(),
            cursor_out: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn stamp(&mut self, device: u8, kind: u8, code: u16, value: i32) -> RawPad {
        self.seq += 1;
        RawPad {
            seq: self.seq,
            ts_ns: self.seq * 1000,
            device,
            kind,
            code,
            value,
        }
    }

    /// A bus record, routed as the service does.
    fn bus(&mut self, device: u8, kind: u8, code: u16, value: i32) -> Route {
        let record = self.stamp(device, kind, code, value);
        let route = self.pads.route(device, kind);
        match route {
            Route::Pad => {
                self.pads.feed(record, &mut self.pad_out);
            }
            Route::Pointer => self.cursor.apply(
                RawPointer {
                    seq: record.seq,
                    ts_ns: record.ts_ns,
                    device,
                    kind,
                    code,
                    value,
                },
                &mut self.cursor_out,
            ),
            Route::Drop => {}
        }
        route
    }

    fn button(&mut self, device: u8, usage: u16, pressed: bool) -> Route {
        self.bus(device, raw::BUTTON, usage, i32::from(pressed))
    }

    /// The kernel's `SOURCE` record.
    fn source(&mut self, device: u8, class: u8, registered: bool) {
        let record = self.stamp(device, raw::SOURCE, u16::from(class), i32::from(registered));
        self.pads
            .source(record, &mut self.pad_out, &mut self.notices);
    }

    /// A read of the kernel's source table.
    fn table(&mut self, entries: &[(u8, u8, u16)]) {
        let entries: Vec<SourceEntry> = entries
            .iter()
            .map(|&(device, class, generation)| SourceEntry {
                device,
                class,
                generation,
            })
            .collect();
        self.seq += 1;
        self.pads.table(
            &entries,
            self.seq * 1000,
            self.seq,
            &mut self.pad_out,
            &mut self.notices,
        );
    }

    /// A `Dropped` record: resync, then the table.
    fn dropped(&mut self, entries: &[(u8, u8, u16)]) {
        self.seq += 1;
        self.pads
            .resync(self.seq * 1000, self.seq, &mut self.pad_out);
        self.table(entries);
    }

    fn notices(&mut self) -> Vec<Notice> {
        core::mem::take(&mut self.notices)
    }

    fn pad_events(&mut self) -> Vec<PadOut> {
        self.pads.flush(&mut self.pad_out);
        core::mem::take(&mut self.pad_out)
    }
}

fn released(events: &[PadOut], usage: u16) -> bool {
    events
        .iter()
        .any(|event| matches!(event, PadOut::Button { code, pressed: false, .. } if *code == usage))
}

#[test]
fn a_pads_buttons_leave_the_cursor_alone() {
    let mut rig = Rig::new();
    rig.source(5, GAMEPAD_CLASS, true);
    assert_eq!(rig.notices(), [Notice::Attached(5)]);
    for usage in 1..=5 {
        assert_eq!(rig.button(5, usage, true), Route::Pad);
    }
    assert_eq!(rig.cursor.buttons(), 0);
    assert!(rig.cursor_out.is_empty());
    let events = rig.pad_events();
    assert_eq!(events.len(), 5);
    assert_eq!(rig.pads.engine().pad(5).unwrap().buttons, 0b11111);
    // And a real mouse still clicks.
    rig.source(2, POINTER, true);
    assert_eq!(rig.button(2, 1, true), Route::Pointer);
    assert_eq!(rig.cursor.buttons(), 1);
}

#[test]
fn a_pad_cannot_move_the_cursor_or_scroll() {
    let mut rig = Rig::new();
    rig.source(5, GAMEPAD_CLASS, true);
    let before = rig.cursor.position();
    for kind in [
        pointer_raw::REL_MOTION,
        pointer_raw::ABS_MOTION,
        pointer_raw::SCROLL,
    ] {
        assert_eq!(rig.bus(5, kind, 0, 0x0010_0010), Route::Drop);
    }
    assert_eq!(rig.cursor.position(), before);
    assert_eq!(rig.pads.engine().rejected(), 0, "dropped before the engine");
}

#[test]
fn an_unknown_device_keeps_pointer_routing_and_drops_axes() {
    let mut rig = Rig::new();
    // No SOURCE seen (an older kernel, or a failed table read).
    assert_eq!(rig.button(2, 1, true), Route::Pointer);
    assert_eq!(rig.cursor.buttons(), 1);
    assert_eq!(rig.bus(2, pointer_raw::SCROLL, 0, 1), Route::Pointer);
    assert_eq!(rig.bus(2, raw::AXIS, usage::X, 30000), Route::Drop);
    assert_eq!(rig.bus(2, raw::DEVICE, 0x1234, 1), Route::Drop);
    assert!(rig.pad_events().is_empty());
}

#[test]
fn a_known_pointer_drops_axes_and_keeps_its_buttons() {
    let mut rig = Rig::new();
    rig.source(2, POINTER, true);
    rig.source(7, TABLET, true);
    rig.source(8, KEYBOARD, true);
    assert_eq!(rig.bus(2, raw::AXIS, usage::X, 30000), Route::Drop);
    assert_eq!(rig.button(7, 1, true), Route::Pointer);
    assert_eq!(rig.button(2, 2, true), Route::Pointer);
    assert!(rig.notices().is_empty());
}

#[test]
fn closing_a_pad_releases_it_and_frees_the_id_for_a_pointer() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.button(3, 1, true);
    rig.bus(3, raw::AXIS, usage::X, 30000);
    let _ = rig.pad_events();
    rig.source(3, GAMEPAD_CLASS, false);
    let events = rig.pad_events();
    assert!(released(&events, 1));
    assert!(events
        .iter()
        .any(|e| matches!(e, PadOut::Axis { value: 0, .. })));
    assert_eq!(rig.notices().last(), Some(&Notice::Detached(3)));
    assert!(rig.pads.engine().pad(3).is_none());
    // A mouse takes the id: its buttons are the cursor's.
    rig.source(3, POINTER, true);
    assert_eq!(rig.button(3, 1, true), Route::Pointer);
    assert_eq!(rig.cursor.buttons(), 1);
    assert!(rig.pad_events().is_empty());
}

#[test]
fn a_new_source_on_a_known_id_starts_it_over() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.button(3, 4, true);
    // The close record was lost: the next registration shows it.
    rig.source(3, GAMEPAD_CLASS, true);
    assert!(released(&rig.pad_events(), 4));
    assert_eq!(rig.pads.engine().pad(3).unwrap().buttons, 0);
    assert_eq!(
        rig.notices(),
        [
            Notice::Attached(3),
            Notice::Detached(3),
            Notice::Attached(3)
        ]
    );
    // And a pointer registering over a pad's id.
    rig.button(3, 4, true);
    rig.source(3, POINTER, true);
    assert!(released(&rig.pad_events(), 4));
    assert_eq!(rig.button(3, 1, true), Route::Pointer);
}

#[test]
fn source_records_lost_in_a_gap_are_recovered_from_the_table() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.table(&[(3, GAMEPAD_CLASS, 1)]);
    rig.button(3, 2, true);
    rig.notices();
    // The pad was unplugged and a mouse took id 3; both SOURCE records were
    // lost with the overflow, so the service sees only a Dropped and re-reads.
    rig.dropped(&[(3, POINTER, 2)]);
    assert_eq!(rig.notices(), [Notice::Detached(3)]);
    assert!(released(&rig.pad_events(), 2));
    assert_eq!(rig.button(3, 1, true), Route::Pointer, "a mouse again");
    assert_eq!(rig.cursor.buttons(), 1);
}

#[test]
fn a_pad_whose_source_was_lost_is_found_by_the_table() {
    let mut rig = Rig::new();
    rig.source(3, POINTER, true);
    rig.table(&[(3, POINTER, 1)]);
    rig.dropped(&[(3, GAMEPAD_CLASS, 2)]);
    assert_eq!(rig.notices().last(), Some(&Notice::Attached(3)));
    assert_eq!(rig.button(3, 1, true), Route::Pad);
    assert_eq!(rig.cursor.buttons(), 0);
    assert!(rig.cursor_out.is_empty());
}

#[test]
fn a_generation_change_is_a_new_source_even_in_the_same_class() {
    let mut rig = Rig::new();
    rig.table(&[(3, GAMEPAD_CLASS, 1)]);
    rig.button(3, 6, true);
    rig.notices();
    rig.table(&[(3, GAMEPAD_CLASS, 1)]);
    assert!(rig.notices().is_empty(), "same source");
    assert_eq!(rig.pads.engine().pad(3).unwrap().buttons, 1 << 5);
    rig.table(&[(3, GAMEPAD_CLASS, 2)]);
    assert_eq!(rig.notices(), [Notice::Detached(3), Notice::Attached(3)]);
    assert!(released(&rig.pad_events(), 6));
    assert_eq!(rig.pads.engine().pad(3).unwrap().buttons, 0);
}

#[test]
fn a_device_missing_from_the_table_is_gone() {
    let mut rig = Rig::new();
    rig.table(&[(3, GAMEPAD_CLASS, 1), (4, POINTER, 1)]);
    rig.button(3, 1, true);
    rig.notices();
    rig.table(&[(4, POINTER, 1)]);
    assert_eq!(rig.notices(), [Notice::Detached(3)]);
    assert!(released(&rig.pad_events(), 1));
    // Id 3 is unknown again: pointer routing, axes dropped.
    assert_eq!(rig.bus(3, raw::AXIS, usage::X, 1), Route::Drop);
}

#[test]
fn a_source_record_then_the_table_agree_without_churn() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.button(3, 1, true);
    rig.notices();
    // The table was read after the registration; the generation is learned.
    rig.table(&[(3, GAMEPAD_CLASS, 9)]);
    assert!(rig.notices().is_empty());
    assert_eq!(rig.pads.engine().pad(3).unwrap().buttons, 1);
    // A table that disagrees on the class wins.
    rig.table(&[(3, TABLET, 9)]);
    assert_eq!(rig.notices(), [Notice::Detached(3)]);
}

#[test]
fn dropped_centres_pads_that_stay() {
    let mut rig = Rig::new();
    rig.table(&[(3, GAMEPAD_CLASS, 1)]);
    rig.button(3, 1, true);
    rig.bus(3, raw::AXIS, usage::X, 30000);
    rig.bus(3, raw::AXIS, usage::HAT, 0);
    let _ = rig.pad_events();
    rig.notices();
    rig.dropped(&[(3, GAMEPAD_CLASS, 1)]);
    let events = rig.pad_events();
    assert!(released(&events, 1));
    assert!(released(&events, code::DPAD_UP));
    assert!(events
        .iter()
        .any(|e| matches!(e, PadOut::Axis { value: 0, .. })));
    assert!(rig.notices().is_empty(), "the pad is still attached");
    assert_eq!(rig.pads.engine().count(), 1);
}

#[test]
fn a_pad_over_the_limit_never_falls_back_to_the_cursor() {
    let mut rig = Rig::new();
    for device in 0..MAX_PADS as u8 {
        rig.source(device, GAMEPAD_CLASS, true);
    }
    rig.source(9, GAMEPAD_CLASS, true);
    assert_eq!(rig.notices().last(), Some(&Notice::Refused(9)));
    assert_eq!(rig.button(9, 1, true), Route::Pad);
    assert_eq!(rig.cursor.buttons(), 0);
    assert_eq!(rig.pads.engine().rejected(), 1);
    // Its removal is silent: it never had a slot.
    rig.source(9, GAMEPAD_CLASS, false);
    assert!(rig.notices().is_empty());
}

#[test]
fn malformed_source_records_are_rejected() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.notices();
    for (code, value) in [(0u16, 1), (4, 2), (4, -1), (300, 1), (0xFFFF, 0)] {
        let record = rig.stamp(3, raw::SOURCE, code, value);
        rig.pads.source(record, &mut rig.pad_out, &mut rig.notices);
    }
    assert_eq!(rig.pads.classifier().rejected(), 5);
    assert!(rig.notices().is_empty(), "a bad record changes nothing");
    assert!(rig.pads.classifier().is_pad(3));
}
