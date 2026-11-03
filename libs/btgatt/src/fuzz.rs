//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/btgatt.rs`).
//!
//! [`run`] uses the bytes two ways:
//!
//! * as one PDU, parsed as the answer to every kind of request and as an
//!   unsolicited PDU;
//! * as a script for a [`Hogp`]: a flags octet (bit 0: resume with
//!   `enable` instead of discovering) then `u8` length-prefixed frames, each
//!   delivered as the server's answer. A frame that starts with `0x00` gets
//!   the response opcode of the request in flight, so a script reaches the
//!   deep states instead of stopping at the opcode check.
//!
//! Nothing may panic, the driver never has two requests in flight, and what
//! it reports is within its bounds.

use std::vec::Vec;

use crate::att::{parse_error, parse_incoming, Request, Response};
use crate::hogp::{Hogp, HogpEvent, Step, MAX_CHARS, MAX_REPORT_MAP};
use crate::uuid::Uuid;

pub fn run(data: &[u8]) {
    single(data);
    drive(data);
}

fn requests() -> [Request; 7] {
    [
        Request::ExchangeMtu { mtu: 247 },
        Request::FindInformation {
            start: 1,
            end: 0xFFFF,
        },
        Request::ReadByType {
            start: 1,
            end: 0xFFFF,
            uuid: Uuid::Short(0x2803),
        },
        Request::ReadByGroupType {
            start: 1,
            end: 0xFFFF,
            uuid: Uuid::Short(0x2800),
        },
        Request::Read { handle: 3 },
        Request::ReadBlob {
            handle: 3,
            offset: 22,
        },
        Request::Write {
            handle: 3,
            value: std::vec![1, 0],
        },
    ]
}

/// The ATT requests the client sends (the opcodes `Request::opcode` makes).
fn is_request(pdu: &[u8]) -> bool {
    requests().iter().any(|r| pdu.first() == Some(&r.opcode()))
}

fn single(data: &[u8]) {
    for request in requests() {
        let mut pdu = data.to_vec();
        if let Some(first) = pdu.first_mut() {
            *first = request.opcode() + 1;
        }
        for bytes in [data, pdu.as_slice()] {
            if let Ok(Response::ReadByGroupType(groups)) = Response::parse(&request, bytes, 247) {
                assert!(groups.iter().all(|g| g.start <= g.end));
            }
        }
    }
    let _ = parse_error(data);
    let _ = parse_incoming(data);
}

fn check(step: &Step) {
    let in_flight = step.send.iter().filter(|p| is_request(p)).count();
    assert!(in_flight <= 1, "two requests in flight");
    for event in &step.events {
        if let HogpEvent::Discovered(service) = event {
            assert!(service.report_map.len() <= MAX_REPORT_MAP);
            assert!(service.reports.len() <= MAX_CHARS);
        }
    }
}

/// Run `data` as a script; every event the driver reported.
pub(crate) fn drive(data: &[u8]) -> Vec<HogpEvent> {
    let mut events = Vec::new();
    let Some((&flags, mut rest)) = data.split_first() else {
        return events;
    };
    let mut hogp = Hogp::new(247);
    let mut step = if flags & 1 == 0 {
        hogp.discover(0)
    } else {
        hogp.enable(0, &[0x2C, 0x33], Some(0x29))
    };
    check(&step);
    events.extend(step.events.iter().cloned());
    let mut now = 0u64;
    while let Some((&len, tail)) = rest.split_first() {
        let n = usize::from(len).min(tail.len());
        let (frame, next) = tail.split_at(n);
        rest = next;
        let mut pdu = frame.to_vec();
        let expected = step
            .send
            .iter()
            .rev()
            .find(|p| is_request(p))
            .map(|p| p[0] + 1);
        if let (Some(first), Some(opcode)) = (pdu.first_mut(), expected) {
            if *first == 0 {
                *first = opcode;
            }
        }
        now += 1;
        step = hogp.on_pdu(now, &pdu);
        check(&step);
        events.extend(step.events.iter().cloned());
        // A request in flight always has a deadline.
        if step.send.iter().any(|p| is_request(p)) {
            assert!(hogp.deadline().is_some());
        }
        if hogp.is_failed() {
            break;
        }
    }
    events.extend(hogp.tick(u64::MAX).events);
    events
}
