//! The discovery list under a crowd of advertisers: junk must not lock a real
//! device out, and each discovery starts a fresh list.

use super::ctl::br_rig;
use super::*;
use crate::ctl::MAX_FOUND;
use crate::ctl_types::*;

fn result(last: u8, rssi: i8, name: &[u8]) -> bthci::Event {
    let mut eir = vec![name.len() as u8 + 1, 0x09];
    eir.extend_from_slice(name);
    bthci::inquiry::InquiryEvent::Results(vec![bthci::inquiry::Entry {
        addr: addr([7, 7, 7, 7, 7, last]),
        class_of_device: 0x2540,
        rssi: Some(rssi),
        eir,
    }])
    .to_event(bthci::inquiry::Mode::Extended)
}

#[test]
fn a_full_list_still_takes_a_new_device_and_it_can_be_paired() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 30).unwrap();
    rig.run_for(100);
    for n in 0..MAX_FOUND as u8 + 8 {
        rig.inject_event(result(n, -70, b"Junk"));
    }
    assert_eq!(rig.host.devices().len(), MAX_FOUND);
    rig.inject_event(result(200, -30, b"Real Keyboard"));
    let devices = rig.host.devices();
    assert!(devices.len() <= MAX_FOUND);
    let real = addr([7, 7, 7, 7, 7, 200]);
    assert!(
        devices.iter().any(|d| d.addr == real),
        "the new device is listed"
    );
    // The weakest, oldest junk made room, and subscribers were told.
    assert!(rig
        .device_events
        .iter()
        .any(|(c, _)| *c == DeviceChange::Removed));
    assert_eq!(rig.host.pair(rig.now, real), Ok(()));
}

#[test]
fn a_new_discovery_starts_with_an_empty_list() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 30).unwrap();
    rig.run_for(100);
    for n in 0..MAX_FOUND as u8 {
        rig.inject_event(result(n, -70, b"Junk"));
    }
    assert_eq!(rig.host.devices().len(), MAX_FOUND);
    rig.host.stop_discovery(rig.now);
    rig.run_for(100);
    rig.host.start_discovery(rig.now, 30).unwrap();
    assert!(
        rig.host.devices().is_empty(),
        "last scan's junk is gone: {:?}",
        rig.host.devices()
    );
}
