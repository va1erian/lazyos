//! `Connection` over the `Mock` transport.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::{flags, Encoder, Parcel};

const NAME: &str = "os.lazy.test";
const IFACE: u64 = 0xfeed;

fn connection() -> Connection<Mock> {
    let mock = Mock::new();
    mock.register(NAME);
    Connection::new(mock, NAME)
}

fn call(conn: &Connection<Mock>) -> Result<Parcel, CallError> {
    call_as(conn, Retry::Safe)
}

fn call_as(conn: &Connection<Mock>, retry: Retry) -> Result<Parcel, CallError> {
    conn.call(
        IFACE,
        7,
        vec![1, 2],
        Vec::new(),
        Deadline::millis(500),
        retry,
    )
}

fn refusal(code: u32, message: &str, detail: Option<&Encoder>) -> Parcel {
    let mut body = Encoder::new();
    match detail {
        Some(detail) => body.error_detail(ERROR_FIELD, code, message, detail),
        None => body.error(ERROR_FIELD, code, message),
    }
    .unwrap();
    Parcel {
        body: body.finish(),
        ..Parcel::default()
    }
}

#[test]
fn the_handle_is_resolved_lazily_and_cached() {
    let conn = connection();
    assert!(conn.transport().resolves().is_empty());
    call(&conn).unwrap();
    call(&conn).unwrap();
    call(&conn).unwrap();
    assert_eq!(conn.transport().resolves().len(), 1);
    let calls = conn.transport().calls();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|c| c.handle == calls[0].handle));
}

#[test]
fn the_request_header_is_built_for_the_interface_and_method() {
    let conn = connection();
    call(&conn).unwrap();
    let seen = &conn.transport().calls()[0];
    assert_eq!(seen.header.interface_id, IFACE);
    assert_eq!(seen.header.method, 7);
    assert_eq!(seen.header.version, crate::VERSION);
    assert_eq!(seen.header.flags, 0);
    assert_eq!(seen.body, [1, 2]);
    assert_eq!(seen.until, Some(Deadline::millis(500)));
    conn.send(IFACE, 9, vec![3], Vec::new()).unwrap();
    let sent = &conn.transport().calls()[1];
    assert_eq!(sent.header.flags, flags::ONE_WAY);
    assert_eq!(sent.until, None);
}

#[test]
fn the_first_resolve_waits_the_runtimes_bound_and_try_connect_does_not() {
    let mock = Mock::new().with_connect_bound(Deadline::secs(3));
    let conn = Connection::new(mock, NAME);
    // A wait that reaches its bound is the timeout, a plain resolve is ENOENT.
    assert_eq!(conn.connect(), Err(Errno::TIMEDOUT));
    assert_eq!(conn.try_connect(), Err(Errno::NOENT));
    let resolves = conn.transport().resolves();
    assert_eq!(resolves[0].1, Deadline::secs(3));
    assert_eq!(resolves[1].1, Deadline::Poll);
}

#[test]
fn a_service_that_registers_while_connect_waits_is_connected_in_one_resolve() {
    let mock = Mock::new();
    mock.registers_when_waited_for(NAME);
    let conn = Connection::new(mock, NAME);
    // A poll does not wait, so it does not see the registration.
    assert_eq!(conn.try_connect(), Err(Errno::NOENT));
    let handle = conn.connect().unwrap();
    assert_eq!(conn.connect(), Ok(handle));
    let resolves = conn.transport().resolves();
    assert_eq!(
        resolves.len(),
        2,
        "the wait is one resolve, not a retry loop"
    );
    assert_eq!(resolves[1].1, Deadline::secs(10));
}

#[test]
fn a_dead_peer_is_resolved_again_once_and_the_call_retried_once() {
    let conn = connection();
    call(&conn).unwrap();
    let first = conn.transport().calls()[0].handle;
    // The service restarts: the cached handle is dead, the name is live.
    conn.transport().kill(NAME);
    conn.transport().register(NAME);
    call(&conn).unwrap();
    let calls = conn.transport().calls();
    assert_eq!(calls.len(), 3, "one failed attempt and one retry");
    assert_eq!(calls[1].handle, first);
    assert_ne!(calls[2].handle, first);
    assert_eq!(conn.transport().resolves().len(), 2);
    assert_eq!(conn.transport().released(), [first]);
}

#[test]
fn the_transport_is_told_the_methods_policy() {
    let conn = connection();
    call_as(&conn, Retry::Safe).unwrap();
    call_as(&conn, Retry::Never).unwrap();
    conn.send(IFACE, 9, vec![3], Vec::new()).unwrap();
    let calls = conn.transport().calls();
    assert_eq!(calls[0].retry, Some(Retry::Safe));
    assert_eq!(calls[1].retry, Some(Retry::Never));
    assert_eq!(calls[2].retry, None);
}

#[test]
fn a_safe_call_is_resent_after_the_peer_died_handling_it() {
    let conn = connection();
    call(&conn).unwrap();
    conn.transport().fail(Errno::PIPE);
    call_as(&conn, Retry::Safe).unwrap();
    assert_eq!(
        conn.transport().calls().len(),
        3,
        "the failure and one resend"
    );
    assert_eq!(conn.transport().resolves().len(), 2);
}

#[test]
fn a_call_that_is_never_resent_returns_the_error_and_the_next_call_reconnects() {
    let conn = connection();
    call(&conn).unwrap();
    let first = conn.transport().calls()[0].handle;
    // The peer took the request and died before it replied.
    conn.transport().fail(Errno::PIPE);
    assert_eq!(
        call_as(&conn, Retry::Never),
        Err(CallError::Transport(Errno::PIPE))
    );
    assert_eq!(conn.transport().calls().len(), 2, "no resend");
    assert!(!conn.is_connected());
    assert_eq!(conn.transport().released(), [first]);
    // The next call resolves again and goes through.
    call_as(&conn, Retry::Never).unwrap();
    assert_eq!(conn.transport().resolves().len(), 2);
    assert_ne!(conn.transport().calls()[2].handle, first);
}

#[test]
fn a_call_that_never_left_is_resent_whatever_its_policy() {
    // A stale handle: the kernel refuses the request before it queues it.
    let conn = connection();
    call(&conn).unwrap();
    conn.transport().kill(NAME);
    conn.transport().register(NAME);
    call_as(&conn, Retry::Never).unwrap();
    assert_eq!(conn.transport().calls().len(), 3);
    // The same through a scripted refusal.
    conn.transport().fail_undelivered(Errno::PIPE);
    call_as(&conn, Retry::Never).unwrap();
    assert_eq!(conn.transport().calls().len(), 5);
}

#[test]
fn a_never_call_that_failed_after_delivery_does_not_resolve_again() {
    let conn = connection();
    call(&conn).unwrap();
    conn.transport().fail(Errno::PIPE);
    let before = conn.transport().resolves().len();
    assert_eq!(
        call_as(&conn, Retry::Never),
        Err(CallError::Transport(Errno::PIPE))
    );
    assert_eq!(conn.transport().resolves().len(), before);
}

#[test]
fn a_peer_that_stays_dead_is_tried_twice_and_the_handle_dropped() {
    let conn = connection();
    call(&conn).unwrap();
    conn.transport().kill(NAME);
    assert_eq!(call(&conn), Err(CallError::Transport(Errno::PIPE)));
    assert_eq!(conn.transport().calls().len(), 3, "no retry loop");
    assert!(!conn.is_connected());
    assert_eq!(conn.transport().open_handles(), 0);
    // A reaped name is waited for (nothing is cached after the eviction) and
    // never registers: the wait's timeout.
    conn.transport().unregister(NAME);
    assert_eq!(call(&conn), Err(CallError::Transport(Errno::TIMEDOUT)));
}

#[test]
fn a_oneway_send_reconnects_too() {
    let conn = connection();
    conn.send(IFACE, 1, Vec::new(), Vec::new()).unwrap();
    conn.transport().kill(NAME);
    conn.transport().register(NAME);
    conn.send(IFACE, 1, Vec::new(), Vec::new()).unwrap();
    assert_eq!(conn.transport().resolves().len(), 2);
}

#[test]
fn a_remote_error_is_not_retried_and_keeps_the_connection() {
    let conn = connection();
    conn.transport()
        .reply(refusal(17, "that application is already installed", None));
    let error = call(&conn).unwrap_err();
    assert_eq!(
        error,
        CallError::Remote {
            code: 17,
            message: "that application is already installed".to_string(),
            detail: Detail::default(),
        }
    );
    assert_eq!(error.code(), -17);
    assert_eq!(conn.transport().calls().len(), 1);
    assert_eq!(conn.transport().resolves().len(), 1);
    assert!(conn.is_connected());
}

#[test]
fn an_error_field_with_a_detail_record_is_decoded() {
    let mut detail = Encoder::new();
    detail.string(1, "confd").unwrap();
    detail.string(2, "check the path").unwrap();
    detail.string(3, "err.confd.path").unwrap();
    let conn = connection();
    conn.transport()
        .reply(refusal(1002, "bad path", Some(&detail)));
    match call(&conn).unwrap_err() {
        CallError::Remote {
            code,
            message,
            detail,
        } => {
            assert_eq!(code, 1002);
            assert_eq!(message, "bad path");
            assert_eq!(detail.domain, "confd");
            assert_eq!(detail.hint, "check the path");
            assert_eq!(detail.docs, "err.confd.path");
        }
        other => panic!("not a remote error: {other:?}"),
    }
}

#[test]
fn a_partial_detail_leaves_the_other_parts_empty() {
    let mut detail = Encoder::new();
    detail.string(2, "try again").unwrap();
    let reply = refusal(11, "busy", Some(&detail));
    match Connection::<Mock>::check(reply).unwrap_err() {
        CallError::Remote { detail, .. } => {
            assert_eq!(detail.hint, "try again");
            assert!(detail.domain.is_empty() && detail.docs.is_empty());
        }
        other => panic!("not a remote error: {other:?}"),
    }
}

#[test]
fn a_reply_without_the_error_field_passes_through() {
    let mut body = Encoder::new();
    body.u32(1, 5).unwrap();
    let reply = Parcel {
        body: body.finish(),
        ..Parcel::default()
    };
    let conn = connection();
    conn.transport().reply(reply.clone());
    assert_eq!(call(&conn).unwrap(), reply);
}

#[test]
fn a_deadline_that_expires_is_a_timeout_and_is_not_retried() {
    let conn = connection();
    conn.transport().fail(Errno::TIMEDOUT);
    let error = call(&conn).unwrap_err();
    assert!(error.is_timeout());
    assert_eq!(error.code(), -110);
    assert_eq!(conn.transport().calls().len(), 1);
    assert!(conn.is_connected(), "a slow service is not a dead one");
}

#[test]
fn deadlines_become_kernel_ticks() {
    assert_eq!(Deadline::Never.absolute_ticks(500), 0);
    assert_eq!(Deadline::Poll.absolute_ticks(500), 1);
    assert_eq!(Deadline::millis(500).absolute_ticks(1000), 1050);
    assert_eq!(Deadline::millis(1).absolute_ticks(1000), 1001);
    // Never the values the kernel reads as "forever" or "poll".
    assert_eq!(Deadline::millis(10).absolute_ticks(0), 2);
    assert_eq!(Deadline::Poll.limit(), Some(core::time::Duration::ZERO));
}

#[test]
fn errnos_are_named_and_round_trip_the_syscall_form() {
    assert_eq!(Errno::INVAL.number(), 22);
    assert_eq!(Errno::from_syscall(-32), Errno::PIPE);
    assert_eq!(Errno::PIPE.to_syscall(), -32);
    assert_eq!(Errno::new(2), Errno::NOENT);
    assert_eq!(Errno::TIMEDOUT.name(), Some("TIMEDOUT"));
    assert_eq!(Errno::new(9999).name(), None);
}

#[test]
fn errors_display_friendly_text() {
    let transport: CallError = CallError::Transport(Errno::TIMEDOUT);
    assert_eq!(
        transport.to_string(),
        "the call failed: the service did not answer in time (ETIMEDOUT)"
    );
    let remote = refusal(5, "no such zone", None);
    let remote = Connection::<Mock>::check(remote).unwrap_err();
    assert_eq!(remote.to_string(), "no such zone");
    assert_eq!(remote.message(), "no such zone");
    let service: CallError<&str> = remote.widen();
    assert!(matches!(service, CallError::Remote { .. }));
    let typed: CallError<&str> = CallError::Service("not found");
    assert_eq!(typed.to_string(), "not found");
}

#[test]
fn lift_maps_a_declared_code_and_leaves_the_rest() {
    #[derive(Debug, PartialEq)]
    struct Known;
    let known = |code| (code == 1001).then_some(Known);
    assert_eq!(
        CallError::refusal(1001, "x").lift(known),
        CallError::Service(Known)
    );
    let other = CallError::refusal(5, "text").lift(known);
    assert!(matches!(&other, CallError::Remote { code: 5, message, .. } if message == "text"));
    let down = CallError::Transport(Errno::PIPE).lift(known);
    assert_eq!(down, CallError::Transport(Errno::PIPE));
}

#[test]
fn a_one_way_send_moves_its_objects() {
    let conn = connection();
    let objects = vec![crate::Object::Channel(9)];
    conn.send(IFACE, 3, Vec::new(), objects.clone()).unwrap();
    assert_eq!(conn.transport().calls()[0].objects, objects);
}

#[test]
fn codec_failures_are_distinguished_by_direction() {
    let bad = crate::Error::BadValue;
    assert_eq!(
        CallError::<core::convert::Infallible>::bad_request(bad),
        CallError::Transport(Errno::INVAL)
    );
    assert_eq!(
        CallError::<core::convert::Infallible>::bad_reply(bad),
        CallError::Transport(Errno::BADMSG)
    );
}
