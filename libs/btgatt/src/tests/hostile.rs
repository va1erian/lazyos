use std::vec;

use super::server::keyboard;
use super::{long_map, run};
use crate::att::{op, Request, Response};
use crate::hogp::{Hogp, HogpEvent};
use crate::uuid::Uuid;
use crate::Error;

fn group_request(start: u16, end: u16) -> Request {
    Request::ReadByGroupType {
        start,
        end,
        uuid: Uuid::Short(0x2800),
    }
}

fn parse(request: &Request, pdu: &[u8]) -> Result<Response, Error> {
    Response::parse(request, pdu, 247)
}

#[test]
fn group_entries_must_fill_the_pdu_and_ascend() {
    let req = group_request(1, 0xFFFF);
    // len 6 = start, end, a 16-bit uuid; one good entry.
    let good = [op::READ_BY_GROUP_RSP, 6, 1, 0, 3, 0, 0, 0x18];
    assert!(parse(&req, &good).is_ok());
    // A trailing octet: the entries no longer divide the data.
    let mut ragged = good.to_vec();
    ragged.push(0);
    assert_eq!(parse(&req, &ragged), Err(Error::Malformed));
    // The end before the start.
    assert_eq!(
        parse(&req, &[op::READ_BY_GROUP_RSP, 6, 5, 0, 3, 0, 0, 0x18]),
        Err(Error::Malformed)
    );
    // The second group starts inside the first.
    let overlap = [
        op::READ_BY_GROUP_RSP,
        6,
        1,
        0,
        5,
        0,
        0,
        0x18,
        3,
        0,
        9,
        0,
        1,
        0x18,
    ];
    assert_eq!(parse(&req, &overlap), Err(Error::Malformed));
    // Handle zero, and a group before the range asked.
    assert!(parse(&req, &[op::READ_BY_GROUP_RSP, 6, 0, 0, 3, 0, 0, 0x18]).is_err());
    let later = group_request(10, 0xFFFF);
    assert!(parse(&later, &good).is_err());
    // A zero or tiny entry length, and no entries at all.
    assert!(parse(&req, &[op::READ_BY_GROUP_RSP, 0, 1, 2]).is_err());
    assert!(parse(&req, &[op::READ_BY_GROUP_RSP, 2, 1, 2]).is_err());
    assert!(parse(&req, &[op::READ_BY_GROUP_RSP, 6]).is_err());
}

#[test]
fn find_information_formats_are_checked() {
    let req = Request::FindInformation { start: 1, end: 9 };
    assert!(parse(&req, &[op::FIND_INFO_RSP, 1, 2, 0, 2, 0x29]).is_ok());
    assert!(parse(&req, &[op::FIND_INFO_RSP, 3, 2, 0, 2, 0x29]).is_err());
    assert!(parse(&req, &[op::FIND_INFO_RSP, 1, 2, 0, 2]).is_err());
    assert!(parse(&req, &[op::FIND_INFO_RSP, 1]).is_err());
    // Out of range and not ascending.
    assert!(parse(&req, &[op::FIND_INFO_RSP, 1, 0x0A, 0, 2, 0x29]).is_err());
    assert!(parse(&req, &[op::FIND_INFO_RSP, 1, 4, 0, 2, 0x29, 3, 0, 2, 0x29]).is_err());
}

#[test]
fn a_response_longer_than_the_mtu_is_refused() {
    let req = Request::Read { handle: 3 };
    let mut pdu = vec![op::READ_RSP];
    pdu.extend(vec![0; 23]);
    assert_eq!(Response::parse(&req, &pdu, 23), Err(Error::Malformed));
    assert!(Response::parse(&req, &pdu[..23], 23).is_ok());
}

#[test]
fn a_wrong_opcode_is_unexpected() {
    let req = Request::Read { handle: 3 };
    assert_eq!(parse(&req, &[op::WRITE_RSP]), Err(Error::Unexpected));
    assert_eq!(parse(&req, &[]), Err(Error::Malformed));
}

/// A server that answers every group request with one group that does not
/// advance must not keep the client going.
#[test]
fn discovery_cannot_be_kept_busy_forever() {
    let mut hogp = Hogp::new(247);
    let mut step = hogp.discover(0);
    // Answer the MTU request, then every group request with a group that
    // ends one handle after the last.
    let mut handle = 0u16;
    let mut failed = None;
    for round in 0..2000u32 {
        let Some(pdu) = step.send.pop() else { break };
        let answer = match pdu[0] {
            op::MTU_REQ => vec![op::MTU_RSP, 23, 0],
            op::READ_BY_GROUP_REQ => {
                handle = handle.saturating_add(1);
                let mut r = vec![op::READ_BY_GROUP_RSP, 6];
                r.extend(handle.to_le_bytes());
                r.extend(handle.to_le_bytes());
                r.extend([0x0F, 0x18]);
                r
            }
            _ => break,
        };
        step = hogp.on_pdu(u64::from(round), &answer);
        if let Some(HogpEvent::Failed(e)) = step.events.last() {
            failed = Some(*e);
            break;
        }
    }
    assert_eq!(failed, Some(Error::TooManyRounds));
}

#[test]
fn an_oversized_report_map_is_refused() {
    let mut server = keyboard(&vec![7; 5000]);
    server.mtu = 247;
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    assert_eq!(events, [HogpEvent::Failed(Error::TooLarge)]);
}

#[test]
fn a_characteristic_whose_value_handle_is_outside_the_service_is_refused() {
    let mut server = keyboard(&long_map());
    // Point the HID Information value handle past the end of the service.
    let decl = server.attrs.iter_mut().find(|a| a.handle == 0x22).unwrap();
    decl.value[1] = 0xF0;
    let mut hogp = Hogp::new(247);
    let first = hogp.discover(0);
    let events = run(&mut hogp, &mut server, first);
    assert_eq!(events, [HogpEvent::Failed(Error::Malformed)]);
}

#[test]
fn nothing_is_sent_after_a_failure() {
    let mut hogp = Hogp::new(247);
    let _ = hogp.discover(0);
    let step = hogp.on_pdu(1, &[op::MTU_RSP]);
    assert_eq!(step.events, [HogpEvent::Failed(Error::Malformed)]);
    assert!(hogp.is_failed());
    let after = hogp.on_pdu(2, &[op::MTU_RSP, 23, 0]);
    assert!(after.send.is_empty() && after.events.is_empty());
    assert!(hogp.enable(3, &[1], None).send.is_empty());
}

#[test]
fn a_silent_server_times_the_driver_out() {
    let mut hogp = Hogp::new(247);
    let _ = hogp.discover(100);
    assert_eq!(hogp.deadline(), Some(100 + crate::ATT_TIMEOUT_MS));
    assert!(hogp.tick(100 + crate::ATT_TIMEOUT_MS - 1).events.is_empty());
    let step = hogp.tick(100 + crate::ATT_TIMEOUT_MS);
    assert_eq!(step.events, [HogpEvent::Failed(Error::Timeout)]);
}
