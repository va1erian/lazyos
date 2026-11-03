//! The control surface under hostile input and load: floods of results,
//! malformed events, and many scan/pair/cancel cycles.

use super::ctl::{br_rig, kbd_addr};
use super::*;
use crate::ctl_types::*;

/// An inquiry result a hostile controller could send.
fn result(last: u8, class: u32, name: &[u8]) -> bthci::Event {
    let mut eir = vec![name.len() as u8 + 1, 0x09];
    eir.extend_from_slice(name);
    bthci::inquiry::InquiryEvent::Results(vec![bthci::inquiry::Entry {
        addr: addr([7, 7, 7, 7, 7, last]),
        class_of_device: class,
        rssi: Some(-40),
        eir,
    }])
    .to_event(bthci::inquiry::Mode::Extended)
}

#[test]
fn a_flood_of_results_is_bounded_cleaned_and_filtered() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 30).unwrap();
    rig.run_for(100);
    for n in 0..=255u8 {
        rig.inject_event(result(n, 0x2540, b"Flood"));
    }
    // A phone is not a peripheral and is not listed.
    rig.inject_event(result(1, 0x5A020C, b"Phone"));
    let devices = rig.host.devices();
    assert!(devices.len() <= crate::ctl::MAX_FOUND, "{}", devices.len());
    assert!(rig.host.stats().overflow > 0);
    assert!(devices.iter().all(|d| d.name != "Phone"));
    // Control characters and over-long names are cut before they are shown.
    let mut rig = br_rig().0;
    rig.host.start_discovery(rig.now, 30).unwrap();
    rig.run_for(100);
    let long = [
        b"[31mred
"
        .as_slice(),
        &[b'x'; 200],
    ]
    .concat();
    rig.inject_event(result(9, 0x2540, &long));
    let name = rig
        .host
        .devices()
        .into_iter()
        .find(|d| d.addr == addr([7, 7, 7, 7, 7, 9]))
        .unwrap()
        .name;
    assert!(
        name.chars().count() <= 64 && !name.chars().any(char::is_control),
        "{name:?}"
    );
    // A result when no inquiry runs is ignored.
    let (mut idle, _, _) = br_rig();
    idle.inject_event(result(3, 0x2540, b"Late"));
    assert!(idle.host.devices().is_empty());
}

#[test]
fn malformed_inquiry_events_are_counted_not_trusted() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 30).unwrap();
    rig.run_for(100);
    let before = rig.host.stats().malformed;
    rig.inject_event(bthci::Event::Unknown {
        code: bthci::inquiry::EV_INQUIRY_RESULT,
        params: vec![200, 1, 2, 3],
    });
    rig.inject_event(bthci::Event::Unknown {
        code: bthci::inquiry::EV_EXTENDED_INQUIRY_RESULT,
        params: vec![5],
    });
    assert_eq!(rig.host.stats().malformed, before + 2);
    assert!(rig.host.devices().iter().all(|d| d.addr == kbd_addr()));
}

#[test]
fn many_scan_and_pair_cycles_leave_nothing_behind() {
    let (mut sim, kbd, _) = world();
    sim.enter_pairing_mode(kbd, 0);
    let mut rig = Rig::fresh(sim, Config::default());
    for round in 0..150 {
        rig.host.start_discovery(rig.now, 10).unwrap();
        rig.run_for(1_700);
        if round % 3 == 0 {
            rig.host.pair(rig.now, kbd_addr()).unwrap();
            rig.run_for(100);
            rig.host.cancel_pairing(rig.now);
            rig.run_for(200);
        }
        rig.host.stop_discovery(rig.now);
        rig.run_for(100);
        // Nothing grows: windows closed, no link, no bond, a bounded list.
        assert_eq!(rig.sim.scan_enable(), 2, "round {round}");
        assert_eq!(rig.host.link_count(), 0, "round {round}");
        assert_eq!(rig.host.store().iter().count(), 0);
        assert!(rig.host.devices().len() <= 1);
        assert_eq!(rig.host.stats().overflow, 0);
    }
    rig.assert_clean();
    // And it still pairs afterwards.
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(15_000);
    assert_eq!(rig.host.pairing_state().phase, Phase::Paired);
}
