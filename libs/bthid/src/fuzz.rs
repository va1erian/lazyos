//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/bthid.rs`).
//!
//! [`run`] uses the bytes as: a control-channel message and an
//! interrupt-channel message; a report descriptor (a plan and a decoder are
//! made from it); and a script of reports (`u8` length-prefixed) fed to
//! decoders made from the input itself and from fixed boot descriptors. Held
//! keys must all be released at the end, whatever was fed.

use std::vec::Vec;

use usbhid::boot::KeyEdge;

use crate::decode::{Decoder, Event, Mode, Plan};
use crate::hidp::{parse_control, parse_interrupt};

/// A boot-layout keyboard with report id 1 and a mouse with report id 2.
const KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

pub fn run(data: &[u8]) {
    let _ = parse_control(data);
    let _ = parse_interrupt(data);
    let mut plans = Vec::new();
    plans.extend(Plan::new(data, 0).map(|p| (p, data)));
    plans.extend(Plan::new(KEYBOARD, 0).map(|p| (p, KEYBOARD)));
    for subclass in [0x40, 0x80] {
        plans.extend(Plan::new(&[], subclass).map(|p| (p, &[][..])));
    }
    for (plan, descriptor) in plans {
        feed_script(plan, descriptor, data);
    }
}

fn feed_script(plan: Plan, descriptor: &[u8], script: &[u8]) {
    let mut decoder = Decoder::new(plan, descriptor);
    let mut held = std::collections::BTreeSet::new();
    let mut note = |e: Event| {
        if let Event::Key(KeyEdge { usage, pressed }) = e {
            if pressed {
                held.insert(usage);
            } else {
                held.remove(&usage);
            }
        }
    };
    let mut rest = script;
    while let Some((&len, tail)) = rest.split_first() {
        let n = usize::from(len).min(tail.len());
        let (report, next) = tail.split_at(n);
        rest = next;
        decoder.feed(report, &mut note);
    }
    decoder.release_all(&mut note);
    assert!(held.is_empty(), "keys left down: {held:?}");
    assert!(matches!(plan.mode, Mode::Report | Mode::Boot));
}
