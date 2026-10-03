//! Hub descriptors and port status, and configurations of composite and
//! non-HID devices (every interface listed, boot interfaces anywhere).

use std::vec;
use std::vec::Vec;

use crate::desc::{parse_config, parse_device, Protocol, Transfer, CLASS_HUB, CLASS_MASS_STORAGE};
use crate::hub::{changed_ports, feature, parse_hub, PortSpeed, PortStatus};
use crate::Error;

/// QEMU's `usb-hub` (8 ports, per-port power, 2 ms to power good).
const QEMU_HUB: [u8; 9] = [9, 0x29, 8, 0x0A, 0x00, 1, 0, 0, 0xFF];

#[test]
fn usb2_and_superspeed_hub_descriptors() {
    let hub = parse_hub(&QEMU_HUB, false).unwrap();
    assert_eq!(
        (hub.ports, hub.characteristics, hub.power_on_ms),
        (8, 0x0A, 2)
    );
    assert_eq!(hub.think_time, 0);
    // A high-speed hub with TT think time 3 (bits 5..=6).
    let mut hs = QEMU_HUB;
    hs[3] = 0x60 | 0x01;
    hs[5] = 50;
    let hub = parse_hub(&hs, false).unwrap();
    assert_eq!((hub.think_time, hub.power_on_ms), (3, 100));
    let ss = [12, 0x2A, 4, 0x09, 0x00, 0x32, 0, 0, 0, 0, 0, 0];
    let hub = parse_hub(&ss, true).unwrap();
    assert_eq!((hub.ports, hub.power_on_ms, hub.think_time), (4, 100, 0));
    // Wrong type for the speed, short, zero ports, overlong bLength.
    assert_eq!(parse_hub(&QEMU_HUB, true), Err(Error::Short));
    assert_eq!(parse_hub(&ss, false), Err(Error::WrongType));
    assert_eq!(parse_hub(&QEMU_HUB[..6], false), Err(Error::Short));
    let mut zero = QEMU_HUB;
    zero[2] = 0;
    assert_eq!(parse_hub(&zero, false), Err(Error::BadLength));
    let mut long = QEMU_HUB;
    long[0] = 40;
    assert_eq!(parse_hub(&long, false), Err(Error::BadLength));
}

#[test]
fn port_status_bits() {
    // USB 2: connected, enabled, powered, low speed; connect and reset changed.
    let status = PortStatus::decode(&[0x03, 0x03, 0x11, 0x00], false).unwrap();
    assert!(status.connected() && status.enabled() && status.powered());
    assert_eq!(status.speed(), PortSpeed::Low);
    assert!(status.connect_changed() && status.reset_changed());
    let acks: Vec<u16> = status.change_features().collect();
    assert_eq!(acks, [feature::C_PORT_CONNECTION, feature::C_PORT_RESET]);
    let high = PortStatus::decode(&[0x03, 0x05, 0, 0], false).unwrap();
    assert_eq!(high.speed(), PortSpeed::High);
    let full = PortStatus::decode(&[0x01, 0x01, 0, 0], false).unwrap();
    assert_eq!((full.speed(), full.enabled()), (PortSpeed::Full, false));
    // SuperSpeed: power is bit 9, U0 link state, warm-reset change.
    let ss = PortStatus::decode(&[0x03, 0x02, 0x21, 0x00], true).unwrap();
    assert!(ss.powered() && ss.reset_changed());
    assert_eq!((ss.speed(), ss.link_state()), (PortSpeed::Super, 0));
    let acks: Vec<u16> = ss.change_features().collect();
    assert_eq!(acks, [feature::C_PORT_CONNECTION, feature::C_BH_PORT_RESET]);
    let inactive = PortStatus::decode(&[0xC1, 0x02, 0, 0], true).unwrap();
    assert_eq!(inactive.link_state(), 6);
    assert_eq!(PortStatus::decode(&[1, 2, 3], false), Err(Error::Short));
}

#[test]
fn status_change_bitmaps() {
    let ports: Vec<u8> = changed_ports(&[0b1010_0101, 0b1000_0001], 15).collect();
    assert_eq!(ports, [2, 5, 7, 8, 15], "bit 0 is the hub, not a port");
    let few: Vec<u8> = changed_ports(&[0xFF, 0xFF], 4).collect();
    assert_eq!(few, [1, 2, 3, 4]);
    // A hub claiming 200 ports is driven as 15; a short bitmap is short.
    assert_eq!(changed_ports(&[0xFE, 0xFF, 0xFF], 200).count(), 15);
    assert_eq!(changed_ports(&[0xFE], 15).count(), 7);
}

/// A configuration header for `total` bytes of `count` interfaces.
fn header(total: usize, count: u8) -> Vec<u8> {
    vec![9, 2, total as u8, (total >> 8) as u8, count, 1, 0, 0xA0, 50]
}

#[test]
fn boot_interfaces_need_not_come_first() {
    // Interface 0: a vendor HID (no boot protocol); 1: consumer control
    // (report protocol); 2: the boot keyboard; 3: a boot mouse.
    let mut chain = header(0, 4);
    for (number, subclass, protocol, endpoint) in [
        (0u8, 0u8, 0u8, 0x81u8),
        (1, 0, 0, 0x82),
        (2, 1, 1, 0x83),
        (3, 1, 2, 0x84),
    ] {
        chain.extend_from_slice(&[9, 4, number, 0, 1, 3, subclass, protocol, 0]);
        chain.extend_from_slice(&[9, 0x21, 0x11, 1, 0, 1, 0x22, 40, 0]);
        chain.extend_from_slice(&[7, 5, endpoint, 3, 8, 0, 1]);
    }
    let total = chain.len();
    chain[2..4].copy_from_slice(&(total as u16).to_le_bytes());
    let config = parse_config(&chain).unwrap();
    let boot: Vec<(u8, Protocol, u8)> = config
        .boot_interfaces()
        .map(|i| (i.number, i.protocol, i.endpoint.unwrap().address))
        .collect();
    assert_eq!(
        boot,
        [(2, Protocol::Keyboard, 0x83), (3, Protocol::Mouse, 0x84)]
    );
    assert_eq!(config.first_boot().unwrap().number, 2);
    assert_eq!(config.hid_interfaces().count(), 4);
    assert_eq!(config.first_hid().unwrap().number, 0);
}

#[test]
fn hubs_storage_and_superspeed_companions() {
    // A SuperSpeed hub: interface class 9, interrupt IN with a companion.
    let mut chain = header(0, 1);
    chain.extend_from_slice(&[9, 4, 0, 0, 1, CLASS_HUB, 0, 0, 0]);
    chain.extend_from_slice(&[7, 5, 0x81, 3, 2, 0, 12]);
    chain.extend_from_slice(&[6, 0x30, 0, 0, 2, 0]);
    // Mass storage: bulk IN and OUT, each with a companion (burst 3).
    chain.extend_from_slice(&[9, 4, 1, 0, 2, CLASS_MASS_STORAGE, 6, 0x50, 0]);
    chain.extend_from_slice(&[7, 5, 0x82, 2, 0x00, 0x04, 0]);
    chain.extend_from_slice(&[6, 0x30, 3, 0, 0, 0]);
    chain.extend_from_slice(&[7, 5, 0x02, 2, 0x00, 0x04, 0]);
    chain.extend_from_slice(&[6, 0x30, 3, 0, 0, 0]);
    let total = chain.len();
    chain[2..4].copy_from_slice(&(total as u16).to_le_bytes());
    let config = parse_config(&chain).unwrap();
    let interfaces: Vec<_> = config.interfaces().collect();
    assert_eq!(interfaces.len(), 2);
    let hub = interfaces[0];
    assert_eq!(hub.class, CLASS_HUB);
    let status = hub.endpoint(Transfer::Interrupt, true).unwrap();
    assert_eq!(
        (
            status.max_packet,
            status.interval,
            status.bytes_per_interval
        ),
        (2, 12, 2)
    );
    let storage = interfaces[1];
    assert_eq!(
        (storage.class, storage.subclass, storage.protocol),
        (8, 6, 0x50)
    );
    let bulk_in = storage.endpoint(Transfer::Bulk, true).unwrap();
    let bulk_out = storage.endpoint(Transfer::Bulk, false).unwrap();
    assert_eq!(
        (bulk_in.address, bulk_in.max_packet, bulk_in.max_burst),
        (0x82, 1024, 3)
    );
    assert_eq!((bulk_out.address, bulk_out.max_burst), (0x02, 3));
    assert_eq!(config.hid_interfaces().count(), 0);
    // A short companion is refused like any short record.
    let mut short = chain.clone();
    short[9 + 9 + 7] = 5;
    assert_eq!(parse_config(&short), Err(Error::BadLength));
}

#[test]
fn high_bandwidth_endpoint_bits() {
    let mut chain = header(0, 1);
    chain.extend_from_slice(&[9, 4, 0, 0, 1, 3, 0, 0, 0]);
    // 1024 bytes, two extra transactions per microframe.
    chain.extend_from_slice(&[7, 5, 0x81, 3, 0x00, 0x14, 1]);
    let total = chain.len();
    chain[2..4].copy_from_slice(&(total as u16).to_le_bytes());
    let config = parse_config(&chain).unwrap();
    let endpoint = config.first_hid().unwrap().endpoint.unwrap();
    assert_eq!((endpoint.max_packet, endpoint.extra), (1024, 2));
}

/// Hex to bytes (the harness logs descriptors as hex).
fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

#[test]
fn qemu_full_speed_hub_and_keyboard() {
    // `tools/usb/run.py --hub`: QEMU's usb-hub on a root port, a usb-kbd on
    // its port 1, as `USBD:DESC:*` logged them.
    let hub_device = parse_device(&bytes("12011001090000080904aa55010101020301")).unwrap();
    assert_eq!((hub_device.class, hub_device.max_packet0), (CLASS_HUB, 8));
    let hub_config =
        parse_config(&bytes("09021900010100e000090400000109000000070581030200ff")).unwrap();
    let interface = hub_config.interfaces().next().unwrap();
    let status = interface.endpoint(Transfer::Interrupt, true).unwrap();
    assert_eq!(
        (interface.class, status.max_packet, status.interval),
        (CLASS_HUB, 2, 0xFF)
    );
    // At full speed the keyboard's endpoint 0 is 8 bytes and it polls every
    // 10 ms (its high-speed twin: 64 bytes, bInterval 7).
    let kbd = parse_device(&bytes("120100020000000827060100000001040b01")).unwrap();
    assert_eq!(kbd.max_packet0, 8);
    let config = bytes("09022200010108a032090400000103010100092111010001223f000705810308000a");
    let boot = parse_config(&config).unwrap().first_boot().unwrap();
    assert_eq!(
        (boot.protocol, boot.endpoint.unwrap().interval),
        (Protocol::Keyboard, 10)
    );
}
