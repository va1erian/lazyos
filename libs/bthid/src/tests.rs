//! HIDP framing, plan choice and decoding, with the descriptors of the
//! devices `btsim` presents (written out here so the tests do not depend on
//! the simulator).

use std::vec;
use std::vec::Vec;

use usbhid::boot::{KeyEdge, MouseOut};
use usbhid::report::Out;

use crate::decode::{Decoder, Event, Mode, Plan};
use crate::hidp::{self, Control, FromControl, Handshake};
use crate::Error;

const KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05,
    0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
    0xC0,
];

const MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08,
    0x95, 0x03, 0x81, 0x06, 0xC0, 0xC0,
];

fn events(decoder: &mut Decoder, report: &[u8]) -> Vec<Event> {
    let mut out = Vec::new();
    decoder.feed(report, |e| out.push(e));
    out
}

// ---- HIDP -------------------------------------------------------------------

#[test]
fn control_messages_are_read_by_header() {
    assert_eq!(
        hidp::parse_control(&[0x00]),
        Ok(FromControl::Handshake(Handshake::Success))
    );
    assert_eq!(
        hidp::parse_control(&[0x03]),
        Ok(FromControl::Handshake(Handshake::Unsupported))
    );
    assert_eq!(
        hidp::parse_control(&[0x0F]),
        Ok(FromControl::Handshake(Handshake::Fatal))
    );
    assert_eq!(
        hidp::parse_control(&[0x09]),
        Ok(FromControl::Handshake(Handshake::Unknown))
    );
    assert_eq!(
        hidp::parse_control(&[0x15]),
        Ok(FromControl::Control(Control::VirtualCableUnplug))
    );
    assert_eq!(hidp::parse_control(&[0x16]), Err(Error::Unexpected));
    assert_eq!(
        hidp::parse_control(&[0xA1, 7, 8]),
        Ok(FromControl::Data {
            report_type: 1,
            data: &[7, 8]
        })
    );
    // A device does not send requests on the control channel.
    for head in [0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xF0] {
        assert_eq!(
            hidp::parse_control(&[head]),
            Err(Error::Unexpected),
            "{head:#x}"
        );
    }
    assert_eq!(hidp::parse_control(&[]), Err(Error::Truncated));
}

#[test]
fn the_interrupt_channel_carries_input_reports_only() {
    assert_eq!(hidp::parse_interrupt(&[0xA1, 1, 2, 3]), Ok(&[1, 2, 3][..]));
    assert_eq!(hidp::parse_interrupt(&[0xA1]), Ok(&[][..]));
    assert_eq!(hidp::parse_interrupt(&[0xA2, 1]), Err(Error::Unexpected));
    assert_eq!(hidp::parse_interrupt(&[0xA3, 1]), Err(Error::Unexpected));
    assert_eq!(hidp::parse_interrupt(&[0xB1, 1]), Err(Error::Unexpected));
    assert_eq!(hidp::parse_interrupt(&[0x00]), Err(Error::Unexpected));
    assert_eq!(hidp::parse_interrupt(&[]), Err(Error::Truncated));
}

#[test]
fn what_the_host_sends_has_the_specified_headers() {
    assert_eq!(hidp::set_protocol(true), [0x70]);
    assert_eq!(hidp::set_protocol(false), [0x71]);
    assert_eq!(hidp::hid_control(Control::VirtualCableUnplug), [0x15]);
    assert_eq!(hidp::hid_control(Control::Nop), [0x10]);
    assert_eq!(hidp::output_report(&[1, 2]), [0xA2, 1, 2]);
}

// ---- plans and decoders -----------------------------------------------------

#[test]
fn plans_follow_the_descriptor() {
    let kbd = Plan::new(KEYBOARD, 0x40).unwrap();
    assert_eq!(
        (kbd.mode, kbd.keyboard, kbd.pointer),
        (Mode::Report, true, false)
    );
    let mouse = Plan::new(MOUSE, 0x80).unwrap();
    assert_eq!(
        (mouse.mode, mouse.keyboard, mouse.pointer),
        (Mode::Report, false, true)
    );
    assert!(!mouse.absolute);
    // The subclass is a hint for when the descriptor is useless; it never
    // overrules a descriptor.
    let confused = Plan::new(MOUSE, 0x40).unwrap();
    assert!(confused.pointer && !confused.keyboard);
}

#[test]
fn a_useless_descriptor_falls_back_to_the_boot_protocol_by_subclass() {
    let junk = [0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0xC0];
    let kbd = Plan::new(&junk, 0x40).unwrap();
    assert!(kbd.needs_boot_protocol() && kbd.keyboard && !kbd.pointer);
    let mouse = Plan::new(&junk, 0x80).unwrap();
    assert!(mouse.needs_boot_protocol() && mouse.pointer && !mouse.keyboard);
    // Both bits: a boot device is one thing; the keyboard wins.
    assert!(Plan::new(&junk, 0xC0).unwrap().keyboard);
    assert_eq!(Plan::new(&junk, 0), None);
    assert_eq!(Plan::new(&[], 0), None);
}

#[test]
fn a_combo_descriptor_makes_two_halves_and_the_keyboard_can_be_dropped() {
    let mut combo = KEYBOARD.to_vec();
    combo.extend_from_slice(MOUSE);
    let plan = Plan::new(&combo, 0xC0).unwrap();
    assert!(plan.keyboard && plan.pointer);
    let pointer_only = plan.without_keyboard().unwrap();
    assert!(!pointer_only.keyboard && pointer_only.pointer);
    let mut decoder = Decoder::new(pointer_only, &combo);
    // A keyboard report is not ours now.
    assert!(events(&mut decoder, &[1, 0, 0, 4, 0, 0, 0, 0, 0]).is_empty());
    assert_eq!(
        events(&mut decoder, &[2, 0, 5, 0xFD, 0]),
        [Event::Pointer(Out::Mouse(MouseOut::Motion {
            dx: 5,
            dy: -3
        }))]
    );
    assert_eq!(Plan::new(KEYBOARD, 0).unwrap().without_keyboard(), None);
}

#[test]
fn keyboard_reports_become_edges_and_release_all_lets_go() {
    let plan = Plan::new(KEYBOARD, 0x40).unwrap();
    let mut decoder = Decoder::new(plan, KEYBOARD);
    assert_eq!(
        events(&mut decoder, &[1, 0, 0, 0x04, 0, 0, 0, 0, 0]),
        [Event::Key(KeyEdge {
            usage: 4,
            pressed: true
        })]
    );
    let mut held = Vec::new();
    decoder.release_all(|e| held.push(e));
    assert_eq!(
        held,
        [Event::Key(KeyEdge {
            usage: 4,
            pressed: false
        })]
    );
    decoder.release_all(|_| panic!("nothing is held any more"));
}

#[test]
fn boot_protocol_keyboards_and_mice_decode_without_ids() {
    let junk = [0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0xC0];
    let mut kbd = Decoder::new(Plan::new(&junk, 0x40).unwrap(), &junk);
    assert_eq!(
        events(&mut kbd, &[0, 0, 0x05, 0, 0, 0, 0, 0]),
        [Event::Key(KeyEdge {
            usage: 5,
            pressed: true
        })]
    );
    let mut mouse = Decoder::new(Plan::new(&junk, 0x80).unwrap(), &junk);
    assert_eq!(
        events(&mut mouse, &[1, 3, 0xFE]),
        [
            Event::Pointer(Out::Mouse(MouseOut::Motion { dx: 3, dy: -2 })),
            Event::Pointer(Out::Mouse(MouseOut::Button {
                usage: 1,
                pressed: true
            })),
        ]
    );
    let mut released = Vec::new();
    mouse.release_all(|e| released.push(e));
    assert_eq!(
        released,
        [Event::Pointer(Out::Mouse(MouseOut::Button {
            usage: 1,
            pressed: false
        }))]
    );
    // Too short for either.
    assert!(events(&mut mouse, &[1]).is_empty());
    assert!(!mouse.feed(&[1], |_| {}));
}

#[test]
fn hostile_reports_are_ignored() {
    let mut decoder = Decoder::new(Plan::new(KEYBOARD, 0x40).unwrap(), KEYBOARD);
    for report in [vec![], vec![1], vec![1, 0], vec![9, 0, 0, 4, 0, 0, 0, 0, 0]] {
        assert!(events(&mut decoder, &report).is_empty(), "{report:02x?}");
    }
    // Rollover phantom keys keep the previous state.
    events(&mut decoder, &[1, 0, 0, 4, 0, 0, 0, 0, 0]);
    assert!(events(&mut decoder, &[1, 0, 0, 1, 1, 1, 1, 1, 1]).is_empty());
}

#[test]
fn seeded_fuzz_runs() {
    fuzzkit::for_seeds("bthid_fuzz_run", |_seed, rng| {
        let mut data = match rng.below(3) {
            0 => KEYBOARD.to_vec(),
            1 => MOUSE.to_vec(),
            _ => {
                let n = rng.below(200) as usize;
                rng.bytes(n)
            }
        };
        let flips = rng.below(5) as usize;
        rng.flip_bits(&mut data, flips);
        crate::fuzz::run(&data);
        // The same bytes as a script of reports.
        let mut script = Vec::new();
        for chunk in data.chunks(1 + rng.below(12) as usize) {
            script.push(chunk.len() as u8);
            script.extend_from_slice(chunk);
        }
        crate::fuzz::run(&script);
    });
}
