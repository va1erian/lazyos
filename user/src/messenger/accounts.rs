//! Client and server shapes for the account database (issue #101 companion).
//! See the module doc on [`crate::messenger::accounts`] for the record shape.

use alloc::string::String;

use libmessenger::{Header, Parcel, VERSION};

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
    Ok(wire::decode_authenticate_reply(&reply.body)
        .map_err(Error::Parcel)?
        .ok)
}
