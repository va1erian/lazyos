//! `logind` client and server shapes (issue #101): the session table and the
//! query `messengerctl sessions` renders.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use crate::sys;

use super::{errno, Endpoint, Error, Result, DEFAULT_BUFFER};

/// The login service's registered name.
pub const NAME: &str = "os.lazy.logind";

/// `os.lazy.logind.v1` as an interim eight-byte ABI id.
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.login");

/// `logind` methods.
pub mod method {
    /// Snapshot the session table.
    pub const SESSIONS: u32 = 1;
}

/// `logind` TLV field ids.
pub mod field {
    /// Session id.
    pub const ID: u16 = 2;
    /// Account name.
    pub const USER: u16 = 3;
    /// User id stamped on the session.
    pub const UID: u16 = 4;
    /// Task slot of the session's shell.
    pub const PID: u16 = 5;
    /// `active` or `exited`.
    pub const STATE: u16 = 6;
    /// Tick the session started.
    pub const STARTED: u16 = 7;
    /// Number of active sessions.
    pub const ACTIVE: u16 = 8;
    /// One session record.
    pub const SESSION: u16 = 9;
}

/// How long `fetch_sessions` waits for an answer (PIT ticks).
///
/// `logind` can be sitting at the console prompt when a query arrives, in
/// which case it answers after the next key; a bounded wait keeps a
/// diagnostic tool from hanging forever.
pub const QUERY_DEADLINE_TICKS: u64 = 100;

/// One session row.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct SessionRecord {
    /// Session id minted by `logind`.
    pub id: u64,
    /// Account name.
    pub user: String,
    /// User id.
    pub uid: u32,
    /// Task slot of the session's shell (`0` until spawned).
    pub pid: u64,
    /// `active` while the shell runs, `exited` after it is reaped.
    pub state: String,
    /// Tick the session started.
    pub started: u64,
}

/// A `Sessions` request parcel.
pub fn sessions_request() -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method: method::SESSIONS,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        ..Parcel::default()
    }
}

/// Encode a `Sessions` reply: the active count, then one record per
/// session, oldest first.
pub fn sessions_reply(sessions: &[SessionRecord], active: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::ACTIVE, active).map_err(Error::Parcel)?;
    for session in sessions {
        let mut record = Encoder::new();
        record.u64(field::ID, session.id).map_err(Error::Parcel)?;
        record
            .string(field::USER, &session.user)
            .map_err(Error::Parcel)?;
        record
            .u64(field::UID, session.uid as u64)
            .map_err(Error::Parcel)?;
        record.u64(field::PID, session.pid).map_err(Error::Parcel)?;
        record
            .string(field::STATE, &session.state)
            .map_err(Error::Parcel)?;
        record
            .u64(field::STARTED, session.started)
            .map_err(Error::Parcel)?;
        body.record(field::SESSION, &record)
            .map_err(Error::Parcel)?;
    }
    Ok(Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method: method::SESSIONS,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Decode a `Sessions` reply into `(active, records)`.
pub fn decode_sessions(parcel: &Parcel) -> Result<(u64, Vec<SessionRecord>)> {
    let mut active = 0u64;
    let mut sessions = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        match (field.kind, field.id) {
            (Kind::U64, field::ACTIVE) => {
                active = field.as_u64().map_err(Error::Parcel)?;
            }
            (Kind::Struct, field::SESSION) => {
                let mut nested = field.nested(0).map_err(Error::Parcel)?;
                let mut session = SessionRecord::default();
                while let Some(item) = nested.next().map_err(Error::Parcel)? {
                    match (item.kind, item.id) {
                        (Kind::U64, field::ID) => {
                            session.id = item.as_u64().map_err(Error::Parcel)?
                        }
                        (Kind::String, field::USER) => {
                            session.user = String::from(item.as_str().map_err(Error::Parcel)?)
                        }
                        (Kind::U64, field::UID) => {
                            session.uid = item.as_u64().map_err(Error::Parcel)? as u32
                        }
                        (Kind::U64, field::PID) => {
                            session.pid = item.as_u64().map_err(Error::Parcel)?
                        }
                        (Kind::String, field::STATE) => {
                            session.state = String::from(item.as_str().map_err(Error::Parcel)?)
                        }
                        (Kind::U64, field::STARTED) => {
                            session.started = item.as_u64().map_err(Error::Parcel)?
                        }
                        _ => {}
                    }
                }
                sessions.push(session);
            }
            _ => {}
        }
    }
    Ok((active, sessions))
}

/// Call `logind`'s `Sessions` with a bounded deadline.
pub fn fetch_sessions(endpoint: &Endpoint) -> Result<(u64, Vec<SessionRecord>)> {
    let mut buf = alloc::vec![0u8; DEFAULT_BUFFER];
    let deadline = sys::clock().saturating_add(QUERY_DEADLINE_TICKS);
    let reply = endpoint.call_with(&sessions_request(), &mut buf, Some(deadline))?;
    if reply.header.method != method::SESSIONS {
        return Err(Error::Errno(-errno::EINVAL));
    }
    decode_sessions(&reply)
}
