//! Client and server shapes for the account database (issue #101 companion).
//! See the module doc on [`crate::messenger::accounts`] for the record shape.

use alloc::string::String;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{Endpoint, Error, Result};

/// The generated `os.lazy.accounts.v1` stubs (`idl/accounts.midl`).
pub use messenger_generated::os_lazy_accounts_v1 as wire;

/// One account record, as a lookup reply carries it.
pub use wire::User as UserRecord;

/// A `Create` request's full payload (the initial secret included).
pub use wire::NewUser;

/// The accounts service's registered name.
pub const NAME: &str = "os.lazy.accountsd";

/// The interface id every accounts parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// The structured error field of a refusal ([`error_reply`]). The generated
/// replies use small ids (at most two), so this never collides with one.
const ERROR_FIELD: u16 = 15;

/// Wrap an encoded body in an accounts parcel of `method`.
fn parcel(method: u32, body: alloc::vec::Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        ..Parcel::default()
    }
}

/// A `Lookup` request by name.
pub fn lookup_name_request(name: &str) -> Result<Parcel> {
    let body = wire::encode_lookup_args(&wire::LookupArgs {
        name: Some(String::from(name)),
        uid: None,
    })
    .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_LOOKUP, body))
}

/// A `Lookup` request by uid.
pub fn lookup_uid_request(uid: u32) -> Result<Parcel> {
    let body = wire::encode_lookup_args(&wire::LookupArgs {
        name: None,
        uid: Some(uid),
    })
    .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_LOOKUP, body))
}

/// An `Authenticate` request.
pub fn authenticate_request(name: &str, secret: &str) -> Result<Parcel> {
    let body = wire::encode_authenticate_args(&wire::AuthenticateArgs {
        name: String::from(name),
        secret: String::from(secret),
    })
    .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_AUTHENTICATE, body))
}

/// A `Create` request (an admin's tool would send this).
pub fn create_request(user: &NewUser) -> Result<Parcel> {
    let body = wire::encode_create_args(&wire::CreateArgs { user: user.clone() })
        .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_CREATE, body))
}

/// Encode a `Lookup` reply: `found`, then the record when found.
pub fn user_reply(user: Option<&UserRecord>) -> Result<Parcel> {
    let body = wire::encode_lookup_reply(&wire::LookupReply {
        found: user.is_some(),
        user: user.cloned(),
    })
    .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_LOOKUP, body))
}

/// Encode an `Authenticate` reply.
pub fn auth_reply(matched: bool) -> Result<Parcel> {
    let body = wire::encode_authenticate_reply(&wire::AuthenticateReply { ok: matched })
        .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_AUTHENTICATE, body))
}

/// Encode a `Create` reply with the daemon's detail text.
pub fn create_reply(ok: bool, detail: &str) -> Result<Parcel> {
    let body = wire::encode_create_reply(&wire::CreateReply {
        ok,
        detail: String::from(detail),
    })
    .map_err(Error::Parcel)?;
    Ok(parcel(wire::METHOD_CREATE, body))
}

/// Decode a `Lookup` request into `(name, uid)`; exactly one is expected.
pub fn decode_lookup(parcel: &Parcel) -> Result<(Option<String>, Option<u32>)> {
    let args = wire::decode_lookup_args(&parcel.body).map_err(Error::Parcel)?;
    Ok((args.name, args.uid))
}

/// Decode an `Authenticate` request into `(name, secret)`.
pub fn decode_authenticate(parcel: &Parcel) -> Result<(String, String)> {
    let args = wire::decode_authenticate_args(&parcel.body).map_err(Error::Parcel)?;
    Ok((args.name, args.secret))
}

/// Decode a `Create` request.
pub fn decode_create(parcel: &Parcel) -> Result<NewUser> {
    let args = wire::decode_create_args(&parcel.body).map_err(Error::Parcel)?;
    Ok(args.user)
}

/// Decode a `Lookup` reply into the record, or `None` when not found.
pub fn decode_user(parcel: &Parcel) -> Result<Option<UserRecord>> {
    let reply = wire::decode_lookup_reply(&parcel.body).map_err(Error::Parcel)?;
    Ok(if reply.found { reply.user } else { None })
}

/// The daemon's refusal: an errno-style `code` and a short text, in place of
/// the method's reply. A daemon without accounts (no valid account file,
/// issue #508) answers every request this way, so a caller can tell "no
/// account database" from "no such user"; [`call`] turns it back into
/// [`Error::Errno`].
pub fn error_reply(method: u32, code: i64, text: &str) -> Parcel {
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(ERROR_FIELD, code as u32, text);
    parcel(method, body.finish())
}

/// The structured error field of a reply, when the daemon refused.
fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            let (code, _text) = field.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
}

/// Send `request`, waiting until `deadline` (PIT ticks; `None` = forever), and
/// map a daemon refusal to `Err(Errno(-code))`.
fn call(endpoint: &Endpoint, request: &Parcel, deadline: Option<u64>) -> Result<Parcel> {
    let reply = endpoint.call(request, deadline)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Errno(-code));
    }
    Ok(reply)
}

/// Look a user up by name through the daemon.
pub fn lookup_name(endpoint: &Endpoint, name: &str) -> Result<Option<UserRecord>> {
    let reply = call(endpoint, &lookup_name_request(name)?, None)?;
    decode_user(&reply)
}

/// Look a user up by uid through the daemon.
pub fn lookup_uid(endpoint: &Endpoint, uid: u32) -> Result<Option<UserRecord>> {
    lookup_uid_by(endpoint, uid, None)
}

/// [`lookup_uid`] that gives up at `deadline` (PIT ticks).
pub fn lookup_uid_by(
    endpoint: &Endpoint,
    uid: u32,
    deadline: Option<u64>,
) -> Result<Option<UserRecord>> {
    let reply = call(endpoint, &lookup_uid_request(uid)?, deadline)?;
    decode_user(&reply)
}

/// Ask the daemon whether `secret` belongs to `name`.
pub fn authenticate(endpoint: &Endpoint, name: &str, secret: &str) -> Result<bool> {
    let reply = call(endpoint, &authenticate_request(name, secret)?, None)?;
    Ok(wire::decode_authenticate_reply(&reply.body)
        .map_err(Error::Parcel)?
        .ok)
}

/// The environment of a session's programs (issue #508): `HOME` and `USER`
/// from the account, and `PATH` naming the one program directory. `logind`
/// gives it to the console shell and `init` to every app it launches into a
/// session, so both agree on it.
pub fn session_env(name: &str, home: &str) -> [String; 3] {
    [
        alloc::format!("HOME={home}"),
        alloc::format!("USER={name}"),
        alloc::format!("PATH={}", fhs::SYSTEM_BIN),
    ]
}
