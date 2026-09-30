//! Round-trip tests for the generated `os.lazy.logind.v1` stubs (issue #285).

use messenger_generated::os_lazy_logind_v1::*;

fn session(id: u64, user: &str, state: &str) -> Session {
    Session {
        id,
        user: user.into(),
        uid: 1000 + id as u32,
        pid: 40 + id,
        state: state.into(),
        started: 12_345 * id,
    }
}

#[test]
fn sessions_reply_roundtrips_in_order() {
    let reply = SessionsReply {
        active: 1,
        sessions: vec![session(1, "root", "exited"), session(2, "guest", "active")],
    };
    let body = encode_sessions_reply(&reply).unwrap();
    assert_eq!(decode_sessions_reply(&body).unwrap(), reply);
}

#[test]
fn empty_table_roundtrips() {
    let reply = SessionsReply::default();
    let body = encode_sessions_reply(&reply).unwrap();
    assert_eq!(decode_sessions_reply(&body).unwrap(), reply);
}

#[test]
fn many_sessions_roundtrip() {
    let sessions: Vec<Session> = (0..64).map(|i| session(i, "u", "active")).collect();
    let reply = SessionsReply {
        active: 64,
        sessions,
    };
    let body = encode_sessions_reply(&reply).unwrap();
    assert_eq!(decode_sessions_reply(&body).unwrap(), reply);
}

#[test]
fn truncated_body_is_rejected() {
    let reply = SessionsReply {
        active: 1,
        sessions: vec![session(1, "root", "active")],
    };
    let body = encode_sessions_reply(&reply).unwrap();
    assert!(decode_sessions_reply(&body[..body.len() - 3]).is_err());
}

#[test]
fn login_topic_payloads_roundtrip() {
    let start = LoginStart {
        user: "alice".into(),
        uid: 1000,
        session: 3,
        pid: 42,
        state: "active".into(),
    };
    let body = encode_system_events_login_start(&start).unwrap();
    assert_eq!(decode_system_events_login_start(&body).unwrap(), start);

    let session = LoginSession {
        user: "alice".into(),
        uid: 1000,
        pid: 42,
        state: "exited".into(),
    };
    let body = encode_system_events_login_session(&session).unwrap();
    assert_eq!(decode_system_events_login_session(&body).unwrap(), session);

    let denied = LoginDenied {
        user: "mallory".into(),
        reason: "bad-secret".into(),
    };
    let body = encode_system_events_login_denied(&denied).unwrap();
    assert_eq!(decode_system_events_login_denied(&body).unwrap(), denied);

    let end = LoginEnd {
        user: "alice".into(),
        uid: 1000,
        session: 3,
        status: 0,
    };
    let body = encode_system_events_login_end(&end).unwrap();
    assert_eq!(decode_system_events_login_end(&body).unwrap(), end);
}

#[test]
fn login_topic_payloads_reject_malformed_bodies() {
    for body in [
        encode_system_events_login_start(&LoginStart::default()).unwrap(),
        encode_system_events_login_session(&LoginSession::default()).unwrap(),
        encode_system_events_login_denied(&LoginDenied::default()).unwrap(),
        encode_system_events_login_end(&LoginEnd::default()).unwrap(),
    ] {
        assert!(decode_system_events_login_start(&body[..body.len() - 2]).is_err());
        assert!(decode_system_events_login_start(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }
}

#[test]
fn login_topic_patterns_are_declared() {
    assert_eq!(TOPIC_SYSTEM_EVENTS_LOGIN_START, "system/events/login/start");
    assert_eq!(
        TOPIC_SYSTEM_EVENTS_LOGIN_SESSION,
        "system/events/login/session/+"
    );
    assert_eq!(
        TOPIC_SYSTEM_EVENTS_LOGIN_DENIED,
        "system/events/login/denied"
    );
    assert_eq!(TOPIC_SYSTEM_EVENTS_LOGIN_END, "system/events/login/end");
}
