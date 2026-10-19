//! What `usbd` needs to bind a gamepad (`docs/usb-gamepad-plan.md` G2): the
//! pad probe that keeps a pad from becoming a pointer, and the bus records.

use std::vec::Vec;

use super::golden::{MOUSE_REPORT, PAD_REPORT, PAD_REPORT_4WAY, PAD_REPORT_IDS, TABLET_REPORT};
use crate::busrec::{self, pad_record, BusRecord};
use crate::report::gamepad::{has_pad_collection, parse_gamepad, Decoder, PadOut};
use crate::report::{parse_pointer, usage};

/// A boot-style keyboard report descriptor (Generic Desktop / Keyboard).
const KEYBOARD: [u8; 39] = [
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07,
    0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xc0,
];

#[test]
fn a_pad_collection_is_recognized_whether_or_not_it_can_be_placed() {
    for pad in [&PAD_REPORT[..], &PAD_REPORT_IDS, &PAD_REPORT_4WAY] {
        assert!(has_pad_collection(pad));
        assert!(parse_gamepad(pad).is_ok());
    }
    // No Y: a pad the library cannot place, but still a pad.
    let mut no_y = PAD_REPORT.to_vec();
    no_y[51] = 0x30;
    assert!(parse_gamepad(&no_y).is_err());
    assert!(has_pad_collection(&no_y));
    // Relative axes: not placed either. The pointer parser would take this
    // for a mouse, which is the reason the driver must not fall back to it.
    let mut relative = PAD_REPORT.to_vec();
    relative[65] = 0x06;
    assert!(parse_gamepad(&relative).is_err());
    assert!(has_pad_collection(&relative));
    assert!(
        parse_pointer(&relative).is_ok(),
        "the trap this probe guards against: a relative pad reads as a mouse"
    );
}

#[test]
fn pointers_and_keyboards_are_not_pads() {
    assert!(!has_pad_collection(&TABLET_REPORT));
    assert!(!has_pad_collection(&MOUSE_REPORT));
    assert!(!has_pad_collection(&KEYBOARD));
    assert!(!has_pad_collection(&[]));
    // Axes with no collection around them.
    let bare = [
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    ];
    assert!(!has_pad_collection(&bare));
}

#[test]
fn the_probe_is_bounded_on_truncated_and_hostile_descriptors() {
    for descriptor in [&PAD_REPORT[..], &PAD_REPORT_IDS, &PAD_REPORT_4WAY] {
        // Every prefix: no panic, and once a pad collection has been seen a
        // longer prefix still says pad.
        let mut seen = false;
        for len in 0..=descriptor.len() {
            let pad = has_pad_collection(&descriptor[..len]);
            assert!(pad || !seen, "pad at a shorter prefix, not at {len}");
            seen |= pad;
        }
        assert!(seen);
    }
    for descriptor in [&TABLET_REPORT[..], &MOUSE_REPORT, &KEYBOARD] {
        for len in 0..=descriptor.len() {
            assert!(!has_pad_collection(&descriptor[..len]));
        }
    }
    // A long item whose size runs off the end, and a collection nest deeper
    // than the walk records: both answer, neither panics.
    assert!(!has_pad_collection(&[0xfe, 0xff, 0x00]));
    let mut nested = std::vec![0x05, 0x01, 0x09, 0x05, 0xa1, 0x01];
    for _ in 0..64 {
        nested.extend_from_slice(&[0x05, 0x01, 0x09, 0x01, 0xa1, 0x00]);
    }
    nested.extend_from_slice(&[0x75, 0x08, 0x95, 0x01, 0x81, 0x02]);
    assert!(has_pad_collection(&nested));
}

#[test]
fn decoder_output_maps_to_the_bus_vocabulary() {
    assert_eq!(
        pad_record(PadOut::Axis {
            usage: usage::RX,
            value: -32767
        }),
        BusRecord {
            kind: busrec::AXIS,
            code: 0x33,
            value: -32767
        }
    );
    assert_eq!(
        pad_record(PadOut::Axis {
            usage: usage::HAT,
            value: 8
        }),
        BusRecord {
            kind: busrec::AXIS,
            code: 0x39,
            value: 8
        }
    );
    let button = |usage, pressed| pad_record(PadOut::Button { usage, pressed });
    assert_eq!(button(1, true).value, 1);
    assert_eq!(button(32, false).value, 0);
    assert_eq!((button(32, true).kind, button(32, true).code), (4, 32));

    // A whole report, end to end: one record per change, in decoder order.
    let mut decoder = Decoder::new(parse_gamepad(&PAD_REPORT).unwrap());
    let mut records = Vec::new();
    let report = [0b0000_0011, 0x00, 0x02, 0x81, 0x7f, 0x00, 0x81, 0xff, 0x00];
    assert!(decoder.feed(&report, |out| records.push(pad_record(out))));
    assert_eq!(records.iter().filter(|r| r.kind == busrec::AXIS).count(), 7);
    assert_eq!(
        records
            .iter()
            .filter(|r| r.kind == busrec::BUTTON)
            .map(|r| (r.code, r.value))
            .collect::<Vec<_>>(),
        [(1, 1), (2, 1)]
    );
    // Axes of the report come before its button edges.
    let first_button = records.iter().position(|r| r.kind == busrec::BUTTON);
    assert!(records[..first_button.unwrap()]
        .iter()
        .all(|r| r.kind == busrec::AXIS));
    // Unplugging: sticks to rest, hat centred, buttons released.
    let mut release = Vec::new();
    decoder.release_all(|out| release.push(pad_record(out)));
    assert!(release
        .iter()
        .any(|r| r.kind == busrec::AXIS && r.code == 0x39 && r.value == 8));
    assert!(release
        .iter()
        .filter(|r| r.kind == busrec::AXIS && r.code != 0x39)
        .all(|r| r.value == 0));
    assert_eq!(
        release.iter().filter(|r| r.kind == busrec::BUTTON).count(),
        2
    );
    // Every code a pad can emit is one the kernel's gamepad class admits.
    for record in records.iter().chain(&release) {
        match record.kind {
            busrec::AXIS => assert!((0x30..=0x39).contains(&record.code)),
            _ => assert!((1..=32).contains(&record.code)),
        }
    }
}

#[test]
fn the_device_record_packs_vendor_product_and_version() {
    // The 8BitDo Ultimate 2: vendor 2dc8, product 310b.
    let record = busrec::device_record(0x2dc8, 0x310b, 0x0100);
    assert_eq!(record.kind, busrec::DEVICE);
    assert_eq!(record.code, 0x2dc8);
    assert_eq!(record.value as u32, 0x0100_310b);
    // A release number with the top bit set makes the i32 negative; the
    // halves still come back out.
    let record = busrec::device_record(0xffff, 0xffff, 0x8001);
    assert!(record.value < 0);
    assert_eq!(record.value as u32 & 0xffff, 0xffff);
    assert_eq!(record.value as u32 >> 16, 0x8001);
    assert_eq!(busrec::device_record(0, 0, 0).value, 0);
}
