//! Hostile and malformed input is refused with an error, never a panic.

use alloc::vec::Vec;

use crate::acl;
use crate::command::Command;
use crate::event::Event;
use crate::{Boundary, Error};

#[test]
fn length_octet_must_match() {
    assert_eq!(
        Event::parse(&[0x0E, 0x04, 0x01, 0x03, 0x0C]),
        Err(Error::BadLength)
    );
    assert_eq!(
        Event::parse(&[0x0E, 0x01, 0x01, 0x03, 0x0C, 0x00]),
        Err(Error::BadLength)
    );
    assert_eq!(Command::parse(&[0x03, 0x0C, 0x01]), Err(Error::BadLength));
    assert_eq!(
        Command::parse(&[0x03, 0x0C, 0x00, 0x00]),
        Err(Error::BadLength)
    );
    assert_eq!(Command::parse(&[0x03]), Err(Error::Truncated));
    assert_eq!(Event::parse(&[]), Err(Error::Truncated));
}

#[test]
fn known_events_with_short_bodies_are_errors() {
    // Connection_Complete wants 11 octets.
    for len in 0..11u8 {
        let mut p = alloc::vec![0x03, len];
        p.resize(2 + usize::from(len), 0);
        assert_eq!(Event::parse(&p), Err(Error::Truncated), "len {len}");
    }
}

#[test]
fn every_prefix_of_every_event_is_handled() {
    let samples: Vec<Vec<u8>> = [
        Event::LinkKeyNotification {
            addr: Default::default(),
            key: [1; 16],
            key_type: 4,
        },
        Event::NumberOfCompletedPackets(alloc::vec![(1, 2), (3, 4)]),
        Event::LeLongTermKeyRequest {
            handle: 1,
            random: 2,
            ediv: 3,
        },
    ]
    .iter()
    .map(Event::encode)
    .collect();
    for packet in samples {
        for cut in 0..packet.len() {
            // Fix the length octet so the body, not the frame, is short.
            let mut p = packet[..cut].to_vec();
            if p.len() >= 2 {
                p[1] = (p.len() - 2) as u8;
            }
            assert!(Event::parse(&p).is_err(), "cut {cut}");
        }
    }
}

#[test]
fn number_of_completed_packets_count_is_checked_against_the_body() {
    // Claims 200 entries, carries one.
    let p = [0x13, 0x05, 200, 0x40, 0x00, 0x01, 0x00];
    assert_eq!(Event::parse(&p), Err(Error::Truncated));
}

#[test]
fn advertising_report_limits() {
    // 26 reports.
    let mut p = alloc::vec![0x3E, 0x02, 0x02, 26];
    p[1] = (p.len() - 2) as u8;
    assert_eq!(Event::parse(&p), Err(Error::TooLong));
    // One report with 32 octets of data.
    let mut body = alloc::vec![0x02, 1, 0, 1];
    body.extend_from_slice(&[0; 6]);
    body.push(32);
    body.extend_from_slice(&[0; 33]);
    let mut p = alloc::vec![0x3E, body.len() as u8];
    p.extend_from_slice(&body);
    assert_eq!(Event::parse(&p), Err(Error::TooLong));
    // A report whose data length points past the packet.
    let mut body = alloc::vec![0x02, 1, 0, 1];
    body.extend_from_slice(&[0; 6]);
    body.push(31);
    let mut p = alloc::vec![0x3E, body.len() as u8];
    p.extend_from_slice(&body);
    assert_eq!(Event::parse(&p), Err(Error::Truncated));
}

#[test]
fn acl_length_must_match_and_handle_is_masked() {
    assert_eq!(
        acl::parse(&[0x40, 0x20, 0x05, 0x00, 1]).map(|_| ()),
        Err(Error::BadLength)
    );
    assert_eq!(
        acl::parse(&[0x40, 0x20, 0x00]).map(|_| ()),
        Err(Error::Truncated)
    );
    let (h, _) = acl::parse(&[0xFF, 0xFF, 0x00, 0x00]).unwrap();
    assert_eq!(h.handle, 0x0FFF);
    assert_eq!(h.boundary, Boundary::Reserved);
    assert_eq!(
        acl::encode(1, Boundary::Continuing, &alloc::vec![0; 70_000]),
        Err(Error::TooLong)
    );
}

#[test]
fn commands_with_trailing_or_missing_parameters_are_errors() {
    assert!(Command::parse(&[0x1A, 0x0C, 0x00]).is_err());
    assert!(Command::parse(&[0x1A, 0x0C, 0x02, 0x02, 0x00]).is_err());
    assert!(
        Command::parse(&[0x13, 0x0C, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err()
    );
}

#[test]
fn byte_strings_never_panic() {
    let mut rng = fuzzkit::Rng::new(7);
    for _ in 0..5_000 {
        let n = rng.below(64) as usize;
        let data = rng.bytes(n);
        crate::fuzz::run(&data);
    }
}

#[test]
fn seeded_fuzz_runs() {
    fuzzkit::for_seeds("bthci_fuzz_run", |_seed, rng| {
        let n = rng.below(300) as usize;
        let mut data = rng.bytes(n);
        // Make some inputs look like real events so deeper paths run.
        if let Some(first) = data.first_mut() {
            *first = *rng.pick(&[0x0E, 0x0F, 0x13, 0x3E, 0x05, 0x03]);
        }
        crate::fuzz::run(&data);
    });
}
