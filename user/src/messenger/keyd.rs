//! `keyd` client and wire shapes (issue #102). See the module doc on
//! [`crate::messenger::keyd`] for the "no verifier or key material crosses
//! the channel" contract.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{errno, registry, Endpoint, Error, Result};

/// Registered service name.
pub const NAME: &str = "os.lazy.keyd";

/// `os.lazy.keyd.v1`'s interim eight-byte ABI id (the pattern the other
/// interim service interfaces use; a `midlc` hash replaces it when the IDL
/// owns this surface).
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.keyd.");

/// Keyd methods.
pub mod method {
    /// Check a username/password pair against the stored Argon2id verifier.
    pub const VERIFY: u32 = 1;
    /// HMAC-SHA256 `digest` under a stored key; returns the tag.
    pub const SIGN: u32 = 2;
    /// Seal `data` under a stored key; returns an authenticated blob.
    pub const WRAP: u32 = 3;
    /// Open a blob produced by `Wrap`; returns the plaintext.
    pub const UNWRAP: u32 = 4;
    /// Cryptographically strong bytes.
    pub const RANDOM: u32 = 5;
    /// Create a fresh random key of a named type; returns its id.
    pub const GENERATE: u32 = 6;
    /// List key ids, types, and use counters (never material).
    pub const LIST: u32 = 7;
    /// Round-trip probe.
    pub const PING: u32 = 8;
    /// Install (or replace) an account's password verifier. Root only:
    /// the accounts service pushes its database here so `Verify` can
    /// answer for every account, not just the built-in demo one.
    pub const PROVISION: u32 = 9;
}

/// Protocol TLV field ids.
pub mod field {
    /// Account name for `Verify` / `Provision`.
    pub const USER: u16 = 1;
    /// Plaintext secret for `Verify` (crosses the channel; the kernel
    /// stamps the sender so `keyd` can audit who asked).
    pub const SECRET: u16 = 2;
    /// Key id naming a stored key.
    pub const KEY: u16 = 3;
    /// Digest bytes for `Sign`.
    pub const DIGEST: u16 = 4;
    /// Plaintext for `Wrap` / ciphertext blob for `Unwrap`.
    pub const DATA: u16 = 5;
    /// Number of bytes for `Random`.
    pub const LEN: u16 = 6;
    /// Key type name for `Generate`.
    pub const KIND: u16 = 7;
    /// New key id.
    pub const ID: u16 = 8;
    /// Opaque bytes (random output, tag, wrapped blob).
    pub const BYTES: u16 = 9;
    /// Boolean result (`Verify`).
    pub const OK: u16 = 10;
    /// One key-list record.
    pub const ENTRY: u16 = 11;
    /// Key use counter.
    pub const USES: u16 = 12;
    /// Tick of the last use.
    pub const LAST_USE: u16 = 13;
    /// Structured error reply.
    pub const ERROR: u16 = 14;
}

/// Key type `Generate` accepts for HMAC keys (`Sign`).
pub const KIND_HMAC: &str = "hmac";
/// Key type `Generate` accepts for wrapping keys (`Wrap`/`Unwrap`).
pub const KIND_WRAP: &str = "wrap";

/// Largest plaintext, blob or random payload in one request or reply.
/// Sized well below the 16 KiB call buffer so a reply always fits.
pub const MAX_BYTES: usize = 8 * 1024;

/// One row of the key list: identity and counters only, never material.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyInfo {
    /// Opaque key id clients pass back in `Sign`/`Wrap`/`Unwrap`.
    pub id: u64,
    /// Key type name.
    pub kind: String,
    /// Operations this key has served.
    pub uses: u64,
    /// PIT tick of the last use (`0` before the first).
    pub last_use: u64,
}

/// A header for a keyd parcel of `method`.
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

/// Wrap an encoded body in a keyd parcel.
fn request_parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: header(method),
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}

/// `Verify(user, secret)`.
pub fn verify_request(user: &str, secret: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::USER, user).map_err(Error::Parcel)?;
    body.string(field::SECRET, secret).map_err(Error::Parcel)?;
    Ok(request_parcel(method::VERIFY, body))
}

/// `Provision(user, secret)`: root-only; `keyd` derives and stores the
/// Argon2id verifier, and the secret does not outlive the call.
pub fn provision_request(user: &str, secret: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::USER, user).map_err(Error::Parcel)?;
    body.string(field::SECRET, secret).map_err(Error::Parcel)?;
    Ok(request_parcel(method::PROVISION, body))
}

/// `Sign(key, digest)`.
pub fn sign_request(key: u64, digest: &[u8]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::KEY, key).map_err(Error::Parcel)?;
    body.bytes(field::DIGEST, digest).map_err(Error::Parcel)?;
    Ok(request_parcel(method::SIGN, body))
}

/// `Wrap(key, bytes)`.
pub fn wrap_request(key: u64, plaintext: &[u8]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::KEY, key).map_err(Error::Parcel)?;
    body.bytes(field::DATA, plaintext).map_err(Error::Parcel)?;
    Ok(request_parcel(method::WRAP, body))
}

/// `Unwrap(key, blob)`.
pub fn unwrap_request(key: u64, blob: &[u8]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::KEY, key).map_err(Error::Parcel)?;
    body.bytes(field::DATA, blob).map_err(Error::Parcel)?;
    Ok(request_parcel(method::UNWRAP, body))
}

/// `Random(len)`.
pub fn random_request(len: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::LEN, len).map_err(Error::Parcel)?;
    Ok(request_parcel(method::RANDOM, body))
}

/// `GenerateKey(type)`.
pub fn generate_request(kind: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::KIND, kind).map_err(Error::Parcel)?;
    Ok(request_parcel(method::GENERATE, body))
}

/// `List`.
pub fn list_request() -> Parcel {
    request_parcel(method::LIST, Encoder::new())
}

/// `Ping`.
pub fn ping_request() -> Parcel {
    request_parcel(method::PING, Encoder::new())
}

/// An empty reply (Ping, or a successful void operation).
pub fn ok_reply(method: u32) -> Parcel {
    request_parcel(method, Encoder::new())
}

/// A `Verify` reply carrying the boolean verdict.
pub fn bool_reply(method: u32, ok: bool) -> Parcel {
    let mut body = Encoder::new();
    // A fresh encoder has room for one field, so this cannot fail.
    let _ = body.bool(field::OK, ok);
    request_parcel(method, body)
}

/// A reply carrying opaque bytes (tag, blob, random output).
pub fn bytes_reply(method: u32, bytes: &[u8]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.bytes(field::BYTES, bytes).map_err(Error::Parcel)?;
    Ok(request_parcel(method, body))
}

/// A reply carrying a key id.
pub fn id_reply(method: u32, id: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::ID, id).map_err(Error::Parcel)?;
    Ok(request_parcel(method, body))
}

/// A `List` reply: one `ENTRY` record per key.
pub fn keys_reply(keys: &[KeyInfo]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for key in keys {
        let mut record = Encoder::new();
        record.u64(field::KEY, key.id).map_err(Error::Parcel)?;
        record
            .string(field::KIND, &key.kind)
            .map_err(Error::Parcel)?;
        record.u64(field::USES, key.uses).map_err(Error::Parcel)?;
        record
            .u64(field::LAST_USE, key.last_use)
            .map_err(Error::Parcel)?;
        body.record(field::ENTRY, &record).map_err(Error::Parcel)?;
    }
    Ok(request_parcel(method::LIST, body))
}

/// The daemon's error answer: errno-style code plus friendly text. The
/// client turns the code back into [`Error::Errno`].
pub fn error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    request_parcel(method, body)
}

/// The first structured error field, when the reply is a daemon failure.
fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == field::ERROR {
            let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
}

/// The first `u64` field with `id`, if any.
pub fn u64_field(parcel: &Parcel, id: u16) -> Result<Option<u64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::U64 && field.id == id {
            return Ok(Some(field.as_u64().map_err(Error::Parcel)?));
        }
    }
    Ok(None)
}

/// The first string field with `id`, if any.
pub fn string_field(parcel: &Parcel, id: u16) -> Result<Option<String>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(Some(String::from(field.as_str().map_err(Error::Parcel)?)));
        }
    }
    Ok(None)
}

/// The first `Bytes` field with `id`, if any.
pub fn bytes_field(parcel: &Parcel, id: u16) -> Result<Option<Vec<u8>>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Bytes && field.id == id {
            return Ok(Some(field.as_bytes().to_vec()));
        }
    }
    Ok(None)
}

/// The first `bool` field with `id` (default `false`).
pub fn bool_field(parcel: &Parcel, id: u16) -> Result<bool> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Bool && field.id == id {
            return field.as_bool().map_err(Error::Parcel);
        }
    }
    Ok(false)
}

/// Decode a `Verify` reply.
pub fn decode_bool(parcel: &Parcel) -> Result<bool> {
    bool_field(parcel, field::OK)
}

/// Decode a reply carrying a key id.
pub fn decode_id(parcel: &Parcel) -> Result<u64> {
    u64_field(parcel, field::ID)?.ok_or(Error::Errno(-errno::EINVAL))
}

/// Decode a reply carrying opaque bytes.
pub fn decode_bytes(parcel: &Parcel) -> Result<Vec<u8>> {
    bytes_field(parcel, field::BYTES)?.ok_or(Error::Errno(-errno::EINVAL))
}

/// Decode a `List` reply.
pub fn decode_keys(parcel: &Parcel) -> Result<Vec<KeyInfo>> {
    let mut keys = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(record) = decoder.next().map_err(Error::Parcel)? {
        if record.kind != Kind::Struct || record.id != field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).map_err(Error::Parcel)?;
        let mut key = KeyInfo::default();
        while let Some(item) = nested.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::U64, field::KEY) => key.id = item.as_u64().map_err(Error::Parcel)?,
                (Kind::String, field::KIND) => {
                    key.kind = String::from(item.as_str().map_err(Error::Parcel)?)
                }
                (Kind::U64, field::USES) => key.uses = item.as_u64().map_err(Error::Parcel)?,
                (Kind::U64, field::LAST_USE) => {
                    key.last_use = item.as_u64().map_err(Error::Parcel)?
                }
                _ => {}
            }
        }
        keys.push(key);
    }
    Ok(keys)
}

/// A client of the `keyd` service.
///
/// `keyd` may still be registering when a late-booting task resolves it;
/// callers that must not fail (like the boot self-test) retry, while the
/// interactive commands report the friendly "no service" error.
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    /// Resolve [`NAME`] and wrap the service endpoint.
    pub fn connect() -> Result<Client> {
        Ok(Client {
            endpoint: registry::resolve(NAME)?,
        })
    }

    /// Wrap an already-resolved endpoint.
    pub fn from_endpoint(endpoint: Endpoint) -> Client {
        Client { endpoint }
    }

    /// The underlying service endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Run one request as a blocking call and fail on a daemon error reply.
    /// The request parcel already carries its method, so no separate
    /// selector is needed here.
    fn call(&self, request: &Parcel) -> Result<Parcel> {
        let reply = self.endpoint.call(request, None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Errno(-code));
        }
        Ok(reply)
    }

    /// Check a username/password pair inside `keyd`; the verifier never
    /// leaves the service.
    pub fn verify(&self, user: &str, secret: &str) -> Result<bool> {
        let reply = self.call(&verify_request(user, secret)?)?;
        decode_bool(&reply)
    }

    /// Install (or replace) `user`'s password verifier inside `keyd`.
    /// Refused with `-EPERM` unless the caller is uid 0.
    pub fn provision(&self, user: &str, secret: &str) -> Result<()> {
        self.call(&provision_request(user, secret)?).map(|_| ())
    }

    /// HMAC-SHA256 `digest` under the stored key; returns the tag.
    pub fn sign(&self, key: u64, digest: &[u8]) -> Result<Vec<u8>> {
        let reply = self.call(&sign_request(key, digest)?)?;
        decode_bytes(&reply)
    }

    /// Seal `plaintext` under the stored key.
    pub fn wrap(&self, key: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
        let reply = self.call(&wrap_request(key, plaintext)?)?;
        decode_bytes(&reply)
    }

    /// Open a blob produced by [`Client::wrap`] for this key.
    pub fn unwrap(&self, key: u64, blob: &[u8]) -> Result<Vec<u8>> {
        let reply = self.call(&unwrap_request(key, blob)?)?;
        decode_bytes(&reply)
    }

    /// Cryptographically strong bytes.
    pub fn random(&self, len: usize) -> Result<Vec<u8>> {
        let reply = self.call(&random_request(len as u64)?)?;
        decode_bytes(&reply)
    }

    /// Create a fresh random key of `kind`; returns its id.
    pub fn generate(&self, kind: &str) -> Result<u64> {
        let reply = self.call(&generate_request(kind)?)?;
        decode_id(&reply)
    }

    /// Key ids and last-use counters; never key material.
    pub fn keys(&self) -> Result<Vec<KeyInfo>> {
        let reply = self.call(&list_request())?;
        decode_keys(&reply)
    }

    /// Round-trip probe.
    pub fn ping(&self) -> Result<()> {
        self.call(&ping_request())?;
        Ok(())
    }
}
