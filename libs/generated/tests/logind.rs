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
