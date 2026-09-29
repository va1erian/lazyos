//! `logind` client and server shapes (issue #101): the session table and the
//! query `messengerctl sessions` renders.

use alloc::vec::Vec;

use libmessenger::{Header, Parcel, VERSION};

use crate::sys;

use super::{errno, Endpoint, Error, Result, DEFAULT_BUFFER};

/// The generated `os.lazy.logind.v1` stubs (`idl/logind.midl`).
pub use messenger_generated::os_lazy_logind_v1 as wire;

/// One session row (the generated `Session` struct).
pub use wire::Session as SessionRecord;

/// The login service's registered name.
pub const NAME: &str = "os.lazy.logind";

/// The interface id every `logind` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// How long `fetch_sessions` waits for an answer (PIT ticks).
///
/// `logind` can be sitting at the console prompt when a query arrives, in
/// which case it answers after the next key; a bounded wait keeps a
/// diagnostic tool from hanging forever.
pub const QUERY_DEADLINE_TICKS: u64 = 100;

/// A header for a `logind` parcel of `method`.
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: 0,
        interface_id: INTERFACE,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// A `Sessions` request parcel (the method takes no arguments).
pub fn sessions_request() -> Parcel {
    Parcel {
        header: header(wire::METHOD_SESSIONS),
        ..Parcel::default()
    }
}

/// Encode a `Sessions` reply: the active count, then one record per
/// session, oldest first.
pub fn sessions_reply(sessions: &[SessionRecord], active: u64) -> Result<Parcel> {
    let body = wire::encode_sessions_reply(&wire::SessionsReply {
        active,
        sessions: sessions.to_vec(),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(wire::METHOD_SESSIONS),
        body,
        ..Parcel::default()
    })
}

/// Decode a `Sessions` reply into `(active, records)`.
pub fn decode_sessions(parcel: &Parcel) -> Result<(u64, Vec<SessionRecord>)> {
    let reply = wire::decode_sessions_reply(&parcel.body).map_err(Error::Parcel)?;
    Ok((reply.active, reply.sessions))
}

/// Call `logind`'s `Sessions` with a bounded deadline.
pub fn fetch_sessions(endpoint: &Endpoint) -> Result<(u64, Vec<SessionRecord>)> {
    let mut buf = alloc::vec![0u8; DEFAULT_BUFFER];
    let deadline = sys::clock().saturating_add(QUERY_DEADLINE_TICKS);
    let reply = endpoint.call_with(&sessions_request(), &mut buf, Some(deadline))?;
    if reply.header.method != wire::METHOD_SESSIONS {
        return Err(Error::Errno(-errno::EINVAL));
    }
    decode_sessions(&reply)
}
