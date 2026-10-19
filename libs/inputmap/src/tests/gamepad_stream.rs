//! The source table is read ahead of the record stream: records published
//! before the read are still queued when `inputd` has the table in hand, and
//! the table already says what they led to. The table is applied at its place
//! in the stream, so none of those records goes to the wrong engine
//! (`docs/usb-gamepad-plan.md`, decision 6). Also: a pad refused for want of
//! a slot gets one when another pad leaves, and a generation learned from a
//! table is remembered.

use super::gamepad_route::{released, Rig, POINTER};
use crate::gamepad::pads::Notice;
use crate::gamepad::route::{Route, GAMEPAD_CLASS};
use crate::gamepad::{raw, PadOut, MAX_PADS};

/// A pad is pressed and unplugged, all inside the gap between the ring's
/// oldest record and the table read: the queue holds the press, the release
/// and the close, the table no longer lists the pad.
#[test]
fn a_pad_pressed_and_unplugged_ahead_of_the_table_never_clicks() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    let press = rig.stamp(3, raw::BUTTON, 1, 1);
    let release = rig.stamp(3, raw::BUTTON, 1, 0);
    let close = rig.stamp(3, raw::SOURCE, u16::from(GAMEPAD_CLASS), 0);
    // Read after the close was published: the pad is gone from the table.
    rig.hold(&[], rig.seq + 1);
    assert_eq!(rig.deliver(press), Route::Pad);
    assert_eq!(rig.deliver(release), Route::Pad);
    assert_eq!(rig.cursor.buttons(), 0, "the press clicked the cursor");
    assert!(rig.cursor_out.is_empty(), "the pad moved the cursor");
    rig.deliver(close);
    rig.settle();
    assert_eq!(rig.cursor.buttons(), 0);
    assert!(rig.cursor_out.is_empty());
    let events = rig.pad_events();
    assert!(released(&events, 1), "the pad saw its own release");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, PadOut::Button { code: 1, .. }))
            .count(),
        2
    );
    assert_eq!(rig.notices().last(), Some(&Notice::Detached(3)));
    // With the stream drained the id is nobody's: pointer routing again.
    assert_eq!(rig.button(3, 1, true), Route::Pointer);
}

/// The slot is then reused by a mouse before the table is read: the table says
/// pointer already, the queued pad records must still be the pad's.
#[test]
fn a_slot_reused_by_a_pointer_ahead_of_the_table_keeps_the_pads_records() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    let queued = [
        rig.stamp(3, raw::BUTTON, 2, 1),
        rig.stamp(3, raw::BUTTON, 2, 0),
        rig.stamp(3, raw::SOURCE, u16::from(GAMEPAD_CLASS), 0),
        rig.stamp(3, raw::SOURCE, u16::from(POINTER), 1),
        rig.stamp(3, raw::BUTTON, 1, 1),
    ];
    rig.hold(&[(3, POINTER, 7)], rig.seq + 1);
    for record in &queued[..3] {
        rig.deliver(*record);
        assert_eq!(rig.cursor.buttons(), 0, "a pad record reached the cursor");
        assert!(rig.cursor_out.is_empty());
    }
    // The mouse registers, then clicks: those are the cursor's.
    rig.deliver(queued[3]);
    assert_eq!(rig.deliver(queued[4]), Route::Pointer);
    assert_eq!(rig.cursor.buttons(), 1);
    rig.settle();
    // The table agrees with the stream: nothing changed at the end.
    assert!(!rig.pads.classifier().is_pad(3));
    assert_eq!(rig.cursor.buttons(), 1);
    let events = rig.pad_events();
    assert!(events.iter().any(|e| matches!(
        e,
        PadOut::Button {
            code: 2,
            pressed: true,
            ..
        }
    )));
    assert!(released(&events, 2));
}

/// The table applies just before the first record at or beyond its cut, so a
/// registration whose `SOURCE` was lost takes effect exactly there.
#[test]
fn the_table_applies_at_the_first_record_at_or_beyond_its_cut() {
    let mut rig = Rig::new();
    rig.seq = 8;
    rig.hold(&[(3, GAMEPAD_CLASS, 1)], 10);
    assert!(rig.pads.has_held_table());
    // seq 9 is below the cut: the stream alone classifies it (unknown).
    assert_eq!(rig.button(3, 1, true), Route::Pointer);
    assert!(rig.pads.has_held_table(), "applied too early");
    assert_eq!(rig.pads.applied_table(), 0);
    // seq 10 is the cut: the table first, then the record.
    assert_eq!(rig.button(3, 2, true), Route::Pad);
    assert!(!rig.pads.has_held_table());
    assert_eq!(rig.pads.applied_table(), 10);
    assert_eq!(rig.notices(), [Notice::Attached(3)]);
}

/// A drain that ends before any record reaches the cut applies it then.
#[test]
fn the_table_applies_when_the_drain_runs_dry() {
    let mut rig = Rig::new();
    rig.hold(&[(3, GAMEPAD_CLASS, 1)], 100);
    rig.source(5, POINTER, true);
    rig.button(5, 1, true);
    assert!(rig.pads.has_held_table(), "nothing reached the cut");
    assert!(rig.notices().is_empty());
    rig.settle();
    assert!(!rig.pads.has_held_table());
    assert_eq!(rig.notices(), [Notice::Attached(3)]);
    // Nothing held: settling again does nothing.
    rig.settle();
    assert!(rig.notices().is_empty());
}

/// A table read later replaces one still waiting.
#[test]
fn a_newer_table_replaces_the_held_one() {
    let mut rig = Rig::new();
    rig.hold(&[(3, GAMEPAD_CLASS, 1)], 50);
    rig.hold(&[(4, GAMEPAD_CLASS, 1)], 60);
    rig.settle();
    assert_eq!(rig.notices(), [Notice::Attached(4)]);
    assert_eq!(rig.pads.applied_table(), 60);
    assert!(!rig.pads.classifier().is_pad(3));
}

/// A `Dropped` marker: a table whose cut is at or below the marker's `seq`
/// has covered everything before it and applies first; one cut inside or
/// beyond the lost span is not applied by the marker and is replaced by the
/// fresh read the loop makes after it.
#[test]
fn a_dropped_marker_applies_a_table_that_precedes_it() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.button(3, 4, true);
    let _ = rig.pad_events();
    rig.notices();
    // Read at cut 6; the marker comes at seq 6 (the records from 6 on were
    // lost). The table applies before the marker's resync.
    rig.hold(&[], 6);
    rig.pads.reach(6, 6000, &mut rig.pad_out, &mut rig.notices);
    assert_eq!(rig.notices(), [Notice::Detached(3)]);
    assert!(released(&rig.pad_events(), 4));
    assert_eq!(rig.pads.applied_table(), 6);
    // A table with a cut beyond the marker is left for the fresh read.
    rig.source(4, GAMEPAD_CLASS, true);
    rig.hold(&[], 100);
    rig.pads
        .reach(rig.seq + 1, 9000, &mut rig.pad_out, &mut rig.notices);
    assert!(rig.pads.has_held_table());
    rig.hold(&[(4, GAMEPAD_CLASS, 2)], 120);
    rig.settle();
    assert_eq!(rig.pads.applied_table(), 120);
    assert!(rig.pads.classifier().is_pad(4));
}

/// A table that confirms an entry first learned from a `SOURCE` record
/// remembers its generation, so a later change of generation is seen.
#[test]
fn a_generation_learned_from_a_table_is_remembered() {
    let mut rig = Rig::new();
    rig.source(3, GAMEPAD_CLASS, true);
    rig.button(3, 6, true);
    rig.notices();
    rig.table(&[(3, GAMEPAD_CLASS, 9)]);
    assert!(rig.notices().is_empty(), "the same source, confirmed");
    rig.table(&[(3, GAMEPAD_CLASS, 9)]);
    assert!(rig.notices().is_empty());
    assert_eq!(rig.pads.engine().pad(3).unwrap().buttons, 1 << 5);
    // The slot was registered again behind our back (its records were lost).
    rig.table(&[(3, GAMEPAD_CLASS, 10)]);
    assert_eq!(rig.notices(), [Notice::Detached(3), Notice::Attached(3)]);
    assert!(released(&rig.pad_events(), 6));
}

/// The fifth pad has no slot; when one of the four goes, it gets the slot and
/// its next records are delivered. With two waiting, the lowest id is first.
#[test]
fn a_refused_pad_gets_a_slot_when_another_pad_leaves() {
    let mut rig = Rig::new();
    let fifth = MAX_PADS as u8 + 1;
    for device in 1..=fifth {
        rig.source(device, GAMEPAD_CLASS, true);
    }
    assert_eq!(rig.notices().last(), Some(&Notice::Refused(fifth)));
    assert_eq!(rig.button(fifth, 1, true), Route::Pad);
    assert!(rig.pad_events().is_empty(), "a pad without a slot spoke");
    assert!(rig.pads.engine().pad(fifth).is_none());
    // One of the others is unplugged.
    rig.source(2, GAMEPAD_CLASS, false);
    assert_eq!(
        rig.notices(),
        [Notice::Detached(2), Notice::Attached(fifth)]
    );
    // Its records are delivered now.
    assert_eq!(rig.button(fifth, 3, true), Route::Pad);
    assert_eq!(rig.pads.engine().pad(fifth).unwrap().buttons, 1 << 2);
    let events = rig.pad_events();
    assert!(events.iter().any(|e| matches!(
        e,
        PadOut::Button { device, code: 3, pressed: true, .. } if *device == fifth
    )));
    assert_eq!(rig.pads.engine().count(), MAX_PADS);

    // Two waiting: the lowest id gets the first free slot.
    let mut rig = Rig::new();
    for device in [10u8, 11, 12, 13, 30, 20] {
        rig.source(device, GAMEPAD_CLASS, true);
    }
    rig.notices();
    rig.source(11, GAMEPAD_CLASS, false);
    assert_eq!(rig.notices(), [Notice::Detached(11), Notice::Attached(20)]);
    assert!(rig.pads.engine().pad(30).is_none());
    // A pad lost to a table frees a slot the same way.
    rig.table(&[
        (10, GAMEPAD_CLASS, 1),
        (12, GAMEPAD_CLASS, 1),
        (20, GAMEPAD_CLASS, 1),
        (30, GAMEPAD_CLASS, 1),
    ]);
    assert!(rig.pads.engine().pad(30).is_some(), "13 left, 30 follows");
}
