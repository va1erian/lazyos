use std::vec;

use crate::att::{err, error_pdu, op, Request, Response};
use crate::client::{Client, Event, ATT_TIMEOUT_MS};
use crate::uuid::Uuid;
use crate::Error;

fn read(handle: u16) -> Request {
    Request::Read { handle }
}

#[test]
fn one_request_at_a_time() {
    let mut c = Client::new(247);
    assert_eq!(c.send(0, read(3)).unwrap(), [op::READ_REQ, 3, 0]);
    assert_eq!(c.send(1, read(4)), Err(Error::Busy));
    let got = c.on_pdu(&[op::READ_RSP, 9, 9]);
    assert_eq!(
        got.event,
        Event::Response {
            request: read(3),
            response: Response::Read(vec![9, 9])
        }
    );
    assert!(c.send(2, read(4)).is_ok(), "free again after the answer");
}

#[test]
fn an_error_response_ends_the_request_not_the_client() {
    let mut c = Client::new(247);
    c.send(0, read(3)).unwrap();
    let got = c.on_pdu(&error_pdu(op::READ_REQ, 3, err::INSUFFICIENT_ENCRYPTION));
    assert_eq!(
        got.event,
        Event::Error {
            request: read(3),
            handle: 3,
            code: err::INSUFFICIENT_ENCRYPTION
        }
    );
    assert!(!c.is_dead() && !c.is_busy());
}

#[test]
fn an_error_for_another_request_is_a_violation() {
    let mut c = Client::new(247);
    c.send(0, read(3)).unwrap();
    let got = c.on_pdu(&error_pdu(op::WRITE_REQ, 3, 1));
    assert_eq!(got.event, Event::Failed(Error::Malformed));
    assert!(c.is_dead());
    assert_eq!(c.send(1, read(3)), Err(Error::Dead));
}

#[test]
fn thirty_seconds_without_an_answer_kill_the_client() {
    let mut c = Client::new(247);
    c.send(1_000, read(3)).unwrap();
    assert_eq!(c.deadline(), Some(1_000 + ATT_TIMEOUT_MS));
    assert_eq!(c.tick(1_000 + ATT_TIMEOUT_MS - 1), None);
    assert_eq!(c.tick(1_000 + ATT_TIMEOUT_MS), Some(Error::Timeout));
    assert!(c.is_dead());
    assert_eq!(c.deadline(), None);
}

#[test]
fn the_mtu_is_the_smaller_of_the_two_and_never_below_23() {
    for (server, want) in [(517, 100), (50, 50), (5, 23)] {
        let mut c = Client::new(100);
        c.send(0, Request::ExchangeMtu { mtu: 100 }).unwrap();
        let mut rsp = vec![op::MTU_RSP];
        rsp.extend(u16::to_le_bytes(server));
        c.on_pdu(&rsp);
        assert_eq!(c.mtu(), want, "server offered {server}");
    }
}

#[test]
fn a_write_longer_than_the_mtu_is_refused() {
    let mut c = Client::new(247);
    let long = Request::Write {
        handle: 3,
        value: vec![0; 21],
    };
    assert_eq!(c.send(0, long), Err(Error::TooLong));
    assert_eq!(c.write_command(3, &[0; 21]), Err(Error::TooLong));
    assert!(c.write_command(3, &[0; 20]).is_ok());
}

#[test]
fn the_servers_requests_and_indications_are_answered_not_acted_on() {
    let mut c = Client::new(247);
    let got = c.on_pdu(&[op::READ_REQ, 3, 0]);
    assert_eq!(got.event, Event::Ignored);
    assert_eq!(
        got.reply,
        Some(error_pdu(op::READ_REQ, 0, err::REQUEST_NOT_SUPPORTED))
    );
    let got = c.on_pdu(&[op::INDICATE, 3, 0, 1]);
    assert_eq!(got.reply, Some(vec![op::CONFIRM]));
    // Commands and signed writes are dropped silently.
    assert_eq!(c.on_pdu(&[op::WRITE_CMD, 3, 0, 1]).reply, None);
    assert_eq!(c.on_pdu(&[0xD2, 3, 0, 1]).event, Event::Ignored);
}

#[test]
fn a_stray_response_is_ignored() {
    let mut c = Client::new(247);
    assert_eq!(c.on_pdu(&[op::READ_RSP, 1]).event, Event::Ignored);
    c.send(0, read(3)).unwrap();
    // The wrong response for the request: not the one asked for.
    assert_eq!(c.on_pdu(&[op::WRITE_RSP]).event, Event::Ignored);
    assert!(c.is_busy(), "the real answer is still awaited");
}

#[test]
fn uuids_fold_to_their_short_form() {
    let mut long = [
        0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x12, 0x18, 0, 0,
    ];
    assert_eq!(Uuid::from_wire(&long), Some(Uuid::Short(0x1812)));
    long[15] = 1;
    assert!(matches!(Uuid::from_wire(&long), Some(Uuid::Long(_))));
    assert_eq!(Uuid::from_wire(&[1, 2, 3]), None);
}
