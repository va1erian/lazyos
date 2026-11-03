//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/btl2cap.rs`).
//!
//! [`run`] uses the bytes two ways:
//!
//! * as a signalling payload: parsing must not panic, and commands that parse
//!   must encode and parse back unchanged;
//! * as a script for the channel manager. The first octet picks the link kind
//!   (bit 0: LE) and whether the HID PSMs are allowed (bit 1); then records of
//!   `boundary:u8, length:u8, bytes` are fed as ACL fragments, and every
//!   channel that opens gets a payload sent on it. Bounds must hold whatever
//!   the peer sends: at most `max_channels_per_link` channels, no output
//!   larger than the input could justify.

use std::vec::Vec;

use bthci::Boundary;

use crate::manager::{Action, Config, Event};
use crate::{pdu, L2cap, LinkKind};

const HANDLE: u16 = 0x40;

pub fn run(data: &[u8]) {
    round_trip(data);
    let _ = script(data);
}

fn round_trip(data: &[u8]) {
    if let Ok(cmds) = pdu::parse_commands(data) {
        for c in cmds {
            let again = pdu::parse_commands(&c.encode()).expect("an encoded command parses");
            assert_eq!(again, [c]);
        }
    }
    if let Ok((cid, payload)) = pdu::parse_basic(data) {
        assert_eq!(pdu::encode_basic(cid, payload).expect("fits"), data);
    }
}

/// Run the script; returns every event the manager reported.
pub fn script(data: &[u8]) -> Vec<Event> {
    let mut events = Vec::new();
    let Some((&head, mut rest)) = data.split_first() else {
        return events;
    };
    let cfg = Config {
        psms: std::vec![0x11, 0x13],
        ..Config::default()
    };
    let max_channels = cfg.max_channels_per_link;
    let mut l2 = L2cap::new(cfg);
    let kind = if head & 1 != 0 {
        LinkKind::Le
    } else {
        LinkKind::BrEdr
    };
    l2.link_up(HANDLE, kind).expect("room");
    if head & 2 != 0 {
        l2.set_allowed(HANDLE, &[0x11, 0x13]);
    }
    let mut now = 0u64;
    let mut records = 0;
    while let [b, len, tail @ ..] = rest {
        let len = usize::from(*len);
        if tail.len() < len || records >= 256 {
            break;
        }
        records += 1;
        now += 7;
        let boundary = Boundary::from_bits(u16::from(*b));
        let actions = l2.rx_acl(now, HANDLE, boundary, &tail[..len]);
        events.extend(follow_up(&mut l2, now, &actions));
        assert!(l2.open_channels(HANDLE) <= max_channels);
        rest = &tail[len..];
    }
    let _ = l2.tick(now + 1_000_000);
    assert_eq!(
        l2.open_channels(HANDLE),
        0,
        "a long silence closes unfinished channels"
    );
    events
}

/// Use what came back: send on opened channels, check PDUs are well-formed.
fn follow_up(l2: &mut L2cap, now: u64, actions: &[Action]) -> Vec<Event> {
    let mut events = Vec::new();
    for a in actions {
        match a {
            Action::Send { pdu: p, .. } => {
                assert!(pdu::parse_basic(p).is_ok(), "we only emit well-formed PDUs");
            }
            Action::Event(e) => events.push(e.clone()),
        }
    }
    let opened: Vec<u16> = events
        .iter()
        .filter_map(|e| match e {
            Event::Opened { cid, .. } => Some(*cid),
            _ => None,
        })
        .collect();
    for cid in opened {
        let _ = l2.send(HANDLE, cid, b"x");
        let _ = l2.disconnect(now, HANDLE, cid);
    }
    events
}
