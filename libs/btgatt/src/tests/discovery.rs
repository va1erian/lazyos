use std::vec;
use std::vec::Vec;

use super::server::keyboard;
use super::{long_map, run};
use crate::hogp::{kind, HidService, Hogp, HogpEvent, Report};
use crate::Error;

fn discovered(events: &[HogpEvent]) -> &HidService {
    match events.last() {
        Some(HogpEvent::Discovered(s)) => s,
        other => panic!("not discovered: {other:?}"),
    }
}

#[test]
fn finds_the_hid_service_and_skips_the_others() {
    let mut server = keyboard(&long_map());
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    let service = discovered(&events);
    assert_eq!(service.report_map, long_map(), "Read then Read Blob");
    assert_eq!(service.report_map_handle, 0x25);
    assert_eq!(service.protocol_mode_handle, 0x29);
    assert_eq!(service.info_flags, 0x02);
    assert_eq!(
        service.reports,
        [
            Report {
                value: 0x2B,
                cccd: 0x2C,
                report_id: 1,
                kind: kind::INPUT
            },
            Report {
                value: 0x2F,
                cccd: 0,
                report_id: 1,
                kind: kind::OUTPUT
            },
            Report {
                value: 0x32,
                cccd: 0x33,
                report_id: 0,
                kind: kind::BOOT_KEYBOARD_INPUT
            },
        ]
    );
    assert!(service.boot_keyboard() && !service.boot_mouse());
    // The battery CCCD (0x13) was never touched.
    assert!(server.writes.is_empty());
}

#[test]
fn a_map_that_ends_exactly_on_a_full_response_is_complete() {
    // 63 octets fill the first response; the Read Blob at offset 63 is empty.
    let map: Vec<u8> = (0..63u8).collect();
    let mut server = keyboard(&map);
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    assert_eq!(discovered(&events).report_map, map);
}

#[test]
fn a_server_without_exchange_mtu_still_works() {
    let mut server = keyboard(&[0x05, 0x01]);
    server.no_mtu = true;
    server.mtu = 23;
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    assert_eq!(discovered(&events).report_map, [0x05, 0x01]);
}

#[test]
fn enable_writes_each_cccd_and_the_boot_protocol_command() {
    let mut server = keyboard(&[1, 2, 3]);
    let mut hogp = Hogp::new(247);
    let step = hogp.enable(0, &[0x2C, 0x33], Some(0x29));
    let events = run(&mut hogp, &mut server, step);
    assert_eq!(events, [HogpEvent::Ready]);
    assert_eq!(server.commands, [(0x29, vec![0])], "boot protocol, no response");
    assert_eq!(server.writes, [(0x2C, vec![1, 0]), (0x33, vec![1, 0])]);
}

#[test]
fn enable_with_nothing_to_subscribe_is_ready_at_once() {
    let mut hogp = Hogp::new(247);
    let step = hogp.enable(0, &[], None);
    assert!(step.send.is_empty());
    assert_eq!(step.events, [HogpEvent::Ready]);
}

#[test]
fn a_device_without_the_hid_service_is_refused() {
    let mut server = keyboard(&[1]);
    server
        .attrs
        .retain(|a| a.handle < 0x20 && a.handle != 0x20);
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    assert_eq!(events, [HogpEvent::Failed(Error::NoHidService)]);
}

#[test]
fn a_device_without_a_report_map_is_refused() {
    let mut server = keyboard(&[]);
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    assert_eq!(events, [HogpEvent::Failed(Error::NoReportMap)]);
}

#[test]
fn notifications_count_only_after_a_subscription_was_answered() {
    let mut server = keyboard(&[1, 2, 3]);
    let mut hogp = Hogp::new(247);
    let mut pdu = vec![0x1B, 0x2B, 0x00];
    pdu.extend_from_slice(&[0, 0, 4, 0, 0, 0, 0, 0]);
    // Nobody asked for it yet: a device cannot notify before its CCCD was
    // written, so this is not input.
    let step = hogp.on_pdu(0, &pdu);
    assert_eq!(step.events, [HogpEvent::Unsolicited { handle: 0x2B }]);
    let step = hogp.enable(0, &[0x2C], None);
    let events = run(&mut hogp, &mut server, step);
    assert_eq!(events, [HogpEvent::Ready]);
    let step = hogp.on_pdu(1, &pdu);
    assert_eq!(
        step.events,
        [HogpEvent::Notification {
            handle: 0x2B,
            value: pdu[3..].to_vec()
        }]
    );
}
