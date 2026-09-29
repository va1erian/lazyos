//! Client and server shapes for the account database (issue #101 companion).
//! See the module doc on [`crate::messenger::accounts`] for the record shape.

use alloc::string::String;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{errno, Endpoint, Error, Result};

/// The accounts service's registered name.
pub const NAME: &str = "os.lazy.accountsd";

/// `os.lazy.accountsd.v1` as an interim eight-byte ABI id.
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.acct.");

/// Accounts methods.
pub mod method {
    /// Find a user by name or uid.
    pub const LOOKUP: u32 = 1;
    /// Verify a user's secret.
    pub const AUTHENTICATE: u32 = 2;
    /// Create a user (admin only).
    pub const CREATE: u32 = 3;
}

/// Accounts TLV field ids.
pub mod field {
    /// Account name.
    pub const NAME: u16 = 1;
    /// Numeric user id (`0` = root).
    pub const UID: u16 = 2;
    /// Primary group id.
    pub const GID: u16 = 3;
    /// Secret to verify or the new user's initial secret.
    pub const SECRET: u16 = 4;
    /// Home directory.
    pub const HOME: u16 = 5;
    /// Login shell path.
    pub const SHELL: u16 = 6;
    /// Lookup verdict (`1` = the user exists).
    pub const FOUND: u16 = 7;
    /// Generic success verdict.
    pub const OK: u16 = 8;
    /// Human-readable detail for a refusal.
    pub const DETAIL: u16 = 9;
}

/// One account record, as a lookup reply carries it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct UserRecord {
    /// Account name.
    pub name: String,
    /// User id.
    pub uid: u32,
    /// Primary group id.
    pub gid: u32,
    /// Home directory.
    pub home: String,
    /// Login shell.
    pub shell: String,
}

/// A `CreateUser` request's full payload (the initial secret included).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct NewUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub secret: String,
    pub home: String,
    pub shell: String,
}

/// A header for an accounts parcel of `method`.
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

/// Wrap an encoded body in an accounts parcel.
fn parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: header(method),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// A `Lookup` request by name.
pub fn lookup_name_request(name: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::NAME, name).map_err(Error::Parcel)?;
    Ok(parcel(method::LOOKUP, body))
}

/// A `Lookup` request by uid.
pub fn lookup_uid_request(uid: u32) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::UID, uid as u64).map_err(Error::Parcel)?;
    Ok(parcel(method::LOOKUP, body))
}

/// An `Authenticate` request.
pub fn authenticate_request(name: &str, secret: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::NAME, name).map_err(Error::Parcel)?;
    body.string(field::SECRET, secret).map_err(Error::Parcel)?;
    Ok(parcel(method::AUTHENTICATE, body))
}

/// A `CreateUser` request (an admin's tool would send this).
pub fn create_request(user: &NewUser) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::NAME, &user.name)
        .map_err(Error::Parcel)?;
    body.u64(field::UID, user.uid as u64)
        .map_err(Error::Parcel)?;
    body.u64(field::GID, user.gid as u64)
        .map_err(Error::Parcel)?;
    body.string(field::SECRET, &user.secret)
        .map_err(Error::Parcel)?;
    body.string(field::HOME, &user.home)
        .map_err(Error::Parcel)?;
    body.string(field::SHELL, &user.shell)
        .map_err(Error::Parcel)?;
    Ok(parcel(method::CREATE, body))
}

/// Encode a `Lookup` reply: `FOUND`, then the record when found.
pub fn user_reply(user: Option<&UserRecord>) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::FOUND, user.is_some() as u64)
        .map_err(Error::Parcel)?;
    if let Some(user) = user {
        body.string(field::NAME, &user.name)
            .map_err(Error::Parcel)?;
        body.u64(field::UID, user.uid as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::GID, user.gid as u64)
            .map_err(Error::Parcel)?;
        body.string(field::HOME, &user.home)
            .map_err(Error::Parcel)?;
        body.string(field::SHELL, &user.shell)
            .map_err(Error::Parcel)?;
    }
    Ok(parcel(method::LOOKUP, body))
}

/// Encode an `Authenticate` reply.
pub fn auth_reply(matched: bool) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::OK, matched as u64).map_err(Error::Parcel)?;
    Ok(parcel(method::AUTHENTICATE, body))
}

/// Encode a `CreateUser` reply with the daemon's detail text.
pub fn create_reply(ok: bool, detail: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::OK, ok as u64).map_err(Error::Parcel)?;
    body.string(field::DETAIL, detail).map_err(Error::Parcel)?;
    Ok(parcel(method::CREATE, body))
}

/// The first string field with `id`.
pub fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// The first string field with `id`, if any.
pub fn optional_string(parcel: &Parcel, id: u16) -> Option<String> {
    string_field(parcel, id).ok()
}

/// The first `u64` field with `id`, if any.
pub fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// Decode a `Lookup` request into `(name, uid)`; exactly one is set.
pub fn decode_lookup(parcel: &Parcel) -> Result<(Option<String>, Option<u64>)> {
    Ok((
        optional_string(parcel, field::NAME),
        u64_field(parcel, field::UID),
    ))
}

/// Decode an `Authenticate` request.
pub fn decode_authenticate(parcel: &Parcel) -> Result<(String, String)> {
    Ok((
        string_field(parcel, field::NAME)?,
        string_field(parcel, field::SECRET)?,
    ))
}

/// Decode a `CreateUser` request.
pub fn decode_create(parcel: &Parcel) -> Result<NewUser> {
    Ok(NewUser {
        name: string_field(parcel, field::NAME)?,
        uid: u64_field(parcel, field::UID).unwrap_or(0) as u32,
        gid: u64_field(parcel, field::GID).unwrap_or(0) as u32,
        secret: string_field(parcel, field::SECRET)?,
        home: optional_string(parcel, field::HOME).unwrap_or_default(),
        shell: optional_string(parcel, field::SHELL).unwrap_or_default(),
    })
}

/// Decode a `Lookup` reply into the record, or `None` when not found.
pub fn decode_user(parcel: &Parcel) -> Result<Option<UserRecord>> {
    if u64_field(parcel, field::FOUND).unwrap_or(0) == 0 {
        return Ok(None);
    }
    Ok(Some(UserRecord {
        name: string_field(parcel, field::NAME)?,
        uid: u64_field(parcel, field::UID).unwrap_or(0) as u32,
        gid: u64_field(parcel, field::GID).unwrap_or(0) as u32,
        home: optional_string(parcel, field::HOME).unwrap_or_default(),
        shell: optional_string(parcel, field::SHELL).unwrap_or_default(),
    }))
}

/// Look a user up by name through the daemon.
pub fn lookup_name(endpoint: &Endpoint, name: &str) -> Result<Option<UserRecord>> {
    let reply = endpoint.call(&lookup_name_request(name)?, None)?;
    decode_user(&reply)
}

/// Look a user up by uid through the daemon.
pub fn lookup_uid(endpoint: &Endpoint, uid: u32) -> Result<Option<UserRecord>> {
    let reply = endpoint.call(&lookup_uid_request(uid)?, None)?;
    decode_user(&reply)
}

/// Ask the daemon whether `secret` belongs to `name`.
pub fn authenticate(endpoint: &Endpoint, name: &str, secret: &str) -> Result<bool> {
    let reply = endpoint.call(&authenticate_request(name, secret)?, None)?;
    Ok(u64_field(&reply, field::OK).unwrap_or(0) != 0)
}
