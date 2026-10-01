//! Host tests: golden descriptors of the QEMU HID devices, hostile chains,
//! and the boot report decoders.

use std::vec::Vec;

use crate::boot::{parse_mouse, BootKeyboard, BootMouse, KeyEdge, MouseOut, MouseReport};
use crate::desc::{config_total_len, parse_config, parse_device, Endpoint, Protocol};
use crate::Error;

pub(crate) mod golden;
mod report;

use golden::*;

#[test]
fn qemu_device_descriptors() {
    let kbd = parse_device(&KBD_DEVICE).unwrap();
    assert_eq!(
        (
            kbd.usb,
            kbd.max_packet0,
            kbd.vendor,
            kbd.product,
            kbd.configurations
        ),
        (0x0200, 64, 0x0627, 0x0001, 1)
    );
    assert_eq!(kbd.class, 0, "class is per interface");
    assert_eq!(parse_device(&KBD_DEVICE[..17]), Err(Error::Short));
    let mut wrong = KBD_DEVICE;
    wrong[1] = 2;
    assert_eq!(parse_device(&wrong), Err(Error::WrongType));
    wrong = KBD_DEVICE;
    wrong[0] = 9;
    assert_eq!(parse_device(&wrong), Err(Error::BadLength));
}

#[test]
fn qemu_keyboard_config() {
    assert_eq!(config_total_len(&KBD_CONFIG[..9]), Ok(34));
    let config = parse_config(&KBD_CONFIG).unwrap();
    assert_eq!(config.value, 1);
    let hid = config.first_boot().unwrap();
    assert_eq!(hid.protocol, Protocol::Keyboard);
    assert_eq!((hid.number, hid.alternate, hid.report_len), (0, 0, 0x3F));
    assert_eq!(
        hid.endpoint,
        Some(Endpoint {
            address: 0x81,
            max_packet: 8,
            interval: 7
        })
    );
    assert_eq!(hid.endpoint.unwrap().number(), 1);
}

#[test]
fn qemu_mouse_and_tablet_configs() {
    let mouse = parse_config(&MOUSE_CONFIG).unwrap().first_boot().unwrap();
    assert_eq!(mouse.protocol, Protocol::Mouse);
    assert_eq!(
        (mouse.report_len, mouse.endpoint.unwrap().max_packet),
        (52, 4)
    );
    // The tablet has no boot protocol: only `first_hid` finds it.
    let tablet = parse_config(&TABLET_CONFIG).unwrap();
    assert_eq!(tablet.first_boot(), None);
    let hid = tablet.first_hid().unwrap();
    assert_eq!((hid.protocol, hid.report_len), (Protocol::None, 74));
    assert_eq!(hid.endpoint.unwrap().interval, 4);
}

#[test]
fn composite_keyboard_and_mouse() {
    // One configuration, keyboard interface 0 then mouse interface 1.
    let mut chain = std::vec![9, 2, 59, 0, 2, 1, 0, 0xA0, 50];
    chain.extend_from_slice(&KBD_CONFIG[9..]);
    chain.extend_from_slice(&MOUSE_CONFIG[9..]);
    chain[9 + 25 + 2] = 1;
    let config = parse_config(&chain).unwrap();
    let found: Vec<(u8, Protocol)> = config
        .hid_interfaces()
        .map(|hid| (hid.number, hid.protocol))
        .collect();
    assert_eq!(found, [(0, Protocol::Keyboard), (1, Protocol::Mouse)]);
    assert_eq!(config.first_boot().unwrap().number, 0);
}

#[test]
fn hostile_config_chains_are_refused() {
    // wTotalLength past the buffer.
    let mut long = KBD_CONFIG;
    long[2] = 200;
    assert_eq!(parse_config(&long), Err(Error::Short));
    // wTotalLength under the header.
    let mut tiny = KBD_CONFIG;
    tiny[2] = 3;
    assert_eq!(parse_config(&tiny), Err(Error::BadLength));
    // A zero bLength would loop forever without the check.
    let mut zero = KBD_CONFIG;
    zero[9] = 0;
    assert_eq!(parse_config(&zero), Err(Error::BadLength));
    // A record running past wTotalLength.
    let mut overrun = KBD_CONFIG;
    overrun[27] = 9;
    assert_eq!(parse_config(&overrun), Err(Error::BadLength));
    // An endpoint record too short for its fields.
    let mut short_ep = KBD_CONFIG;
    short_ep[2] = 33;
    short_ep[27] = 6;
    assert_eq!(parse_config(&short_ep[..33]), Err(Error::BadLength));
    // Bytes past wTotalLength are ignored.
    let mut padded = KBD_CONFIG.to_vec();
    padded.extend_from_slice(&[0xFF; 7]);
    assert!(parse_config(&padded).is_ok());
}

#[test]
fn endpoints_must_be_interrupt_in_and_belong_to_a_hid_interface() {
    // An OUT endpoint, then an IN bulk endpoint, then the interrupt IN.
    let mut chain = KBD_CONFIG[..27].to_vec();
    chain.extend_from_slice(&[7, 5, 0x02, 3, 8, 0, 10]);
    chain.extend_from_slice(&[7, 5, 0x83, 2, 64, 0, 0]);
    chain.extend_from_slice(&KBD_CONFIG[27..]);
    let total = chain.len() as u16;
    chain[2..4].copy_from_slice(&total.to_le_bytes());
    let hid = parse_config(&chain).unwrap().first_boot().unwrap();
    assert_eq!(hid.endpoint.unwrap().address, 0x81);
    // A non-HID interface's endpoint is not attached to anything.
    let mut storage = KBD_CONFIG;
    storage[14] = 8; // mass storage class
    let config = parse_config(&storage).unwrap();
    assert_eq!(config.hid_interfaces().count(), 0);
}

fn feed(kbd: &mut BootKeyboard, report: &[u8]) -> Vec<(u16, bool)> {
    let mut out = Vec::new();
    kbd.feed(report, |KeyEdge { usage, pressed }| {
        out.push((usage, pressed))
    })
    .unwrap();
    out
}

#[test]
fn keyboard_reports_become_edges() {
    let mut kbd = BootKeyboard::new();
    // Left Shift + A.
    assert_eq!(
        feed(&mut kbd, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]),
        [(0x04, true), (0xE1, true)]
    );
    // Same state: nothing.
    assert!(feed(&mut kbd, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]).is_empty());
    // A and B swapped slots, B added: only B.
    assert_eq!(
        feed(&mut kbd, &[0x02, 0, 0x05, 0x04, 0, 0, 0, 0]),
        [(0x05, true)]
    );
    // Releases come before presses.
    assert_eq!(
        feed(&mut kbd, &[0x00, 0, 0x06, 0, 0, 0, 0, 0]),
        [(0x04, false), (0x05, false), (0xE1, false), (0x06, true)]
    );
    assert!(kbd.is_held(0x06) && !kbd.is_held(0x04));
}

#[test]
fn rollover_reports_keep_the_previous_state() {
    let mut kbd = BootKeyboard::new();
    feed(&mut kbd, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]);
    assert!(feed(&mut kbd, &[0, 0, 1, 1, 1, 1, 1, 1]).is_empty());
    assert_eq!(kbd.rollovers, 1);
    assert!(kbd.is_held(0x04) && kbd.is_held(0x05));
}

#[test]
fn bad_key_bytes_are_dropped_and_counted() {
    let mut kbd = BootKeyboard::new();
    assert_eq!(
        feed(&mut kbd, &[0, 0, 0xF0, 0x04, 0xFF, 0, 0, 0]),
        [(0x04, true)]
    );
    assert_eq!(kbd.rejected, 2);
    assert_eq!(kbd.feed(&[0], |_| {}), Err(Error::Short));
    // Short reports are fine (modifiers only), long ones are cut at 8.
    assert_eq!(feed(&mut kbd, &[0x01, 0]), [(0x04, false), (0xE0, true)]);
    assert_eq!(feed(&mut kbd, &[0x01, 0, 0, 0, 0, 0, 0, 0, 0x05]), []);
    let mut released = Vec::new();
    kbd.release_all(|edge| released.push(edge.usage));
    assert_eq!(released, [0xE0]);
}

#[test]
fn mouse_reports_in_publication_order() {
    assert_eq!(parse_mouse(&[1, 2]), Err(Error::Short));
    let three = parse_mouse(&[0x01, 0xFE, 5]).unwrap();
    assert_eq!(
        three,
        MouseReport {
            buttons: 1,
            dx: -2,
            dy: 5,
            wheel: 0
        }
    );
    let mut mouse = BootMouse::new();
    let mut out = Vec::new();
    mouse.feed(&parse_mouse(&[0x01, 3, 0xFF, 0x01]).unwrap(), |o| {
        out.push(o)
    });
    assert_eq!(
        out,
        [
            MouseOut::Motion { dx: 3, dy: -1 },
            MouseOut::Wheel(1),
            MouseOut::Button {
                usage: 1,
                pressed: true
            },
        ]
    );
    // Bits above the fifth button are ignored; a release-all frees the rest.
    out.clear();
    mouse.feed(&parse_mouse(&[0xF0, 0, 0, 0]).unwrap(), |o| out.push(o));
    assert_eq!(
        out,
        [
            MouseOut::Button {
                usage: 1,
                pressed: false
            },
            MouseOut::Button {
                usage: 5,
                pressed: true
            },
        ]
    );
    out.clear();
    mouse.release_all(|o| out.push(o));
    assert_eq!(
        out,
        [MouseOut::Button {
            usage: 5,
            pressed: false
        }]
    );
    assert_eq!(mouse.held(), 0);
}
