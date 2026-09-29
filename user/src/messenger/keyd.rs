//! `keyd` client and wire shapes (issue #102). See the module doc on
//! [`crate::messenger::keyd`] for the "no verifier or key material crosses
//! the channel" contract.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{errno, registry, Endpoint, Error, Result};

/// The generated `os.lazy.keyd.v1` stubs (see `idl/keyd.midl`).
pub use messenger_generated::os_lazy_keyd_v1 as wire;

/// One row of the key list: identity and counters only, never material.
pub use wire::KeyInfo;

/// Registered service name.
pub const NAME: &str = "os.lazy.keyd";

/// The interface id every `keyd` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Structured error field id in a reply body. The generated fields of every
/// reply use small ids (at most one), so this can never collide with a
/// success payload.
const ERROR_FIELD: u16 = 15;

/// Key type `Generate` accepts for HMAC keys (`Sign`).
pub const KIND_HMAC: &str = "hmac";
/// Key type `Generate` accepts for wrapping keys (`Wrap`/`Unwrap`).
pub const KIND_WRAP: &str = "wrap";

/// Largest plaintext, blob or random payload in one request or reply.
/// Sized well below the 16 KiB call buffer so a reply always fits.
pub const MAX_BYTES: usize = 8 * 1024;

/// A parcel of `method` carrying an already-encoded `body`; also the reply
/// builder for the daemon.
pub fn parcel(method: u32, body: Vec<u8>) -> Parcel {
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
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}

/// An empty reply (Ping, or a successful void operation).
pub fn ok_reply(method: u32) -> Parcel {
    parcel(method, Vec::new())
}

/// The daemon's error answer: errno-style code plus friendly text. The
/// client turns the code back into [`Error::Errno`].
pub fn error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(ERROR_FIELD, code as u32, error.message());
    parcel(method, body.finish())
}

/// The first structured error field, when the reply is a daemon failure.
fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
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
    fn call(&self, method: u32, body: Vec<u8>) -> Result<Parcel> {
        let reply = self.endpoint.call(&parcel(method, body), None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Errno(-code));
        }
        Ok(reply)
    }

    /// Check a username/password pair inside `keyd`; the verifier never
    /// leaves the service.
    pub fn verify(&self, user: &str, secret: &str) -> Result<bool> {
        let body = wire::encode_verify_args(&wire::VerifyArgs {
            user: String::from(user),
            secret: String::from(secret),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_VERIFY, body)?;
        Ok(wire::decode_verify_reply(&reply.body)
            .map_err(Error::Parcel)?
            .ok)
    }

    /// Install (or replace) `user`'s password verifier inside `keyd`.
    /// Refused with `-EPERM` unless the caller is uid 0.
    pub fn provision(&self, user: &str, secret: &str) -> Result<()> {
        let body = wire::encode_provision_args(&wire::ProvisionArgs {
            user: String::from(user),
            secret: String::from(secret),
        })
        .map_err(Error::Parcel)?;
        self.call(wire::METHOD_PROVISION, body).map(|_| ())
    }

    /// HMAC-SHA256 `digest` under the stored key; returns the tag.
    pub fn sign(&self, key: u64, digest: &[u8]) -> Result<Vec<u8>> {
        let body = wire::encode_sign_args(&wire::SignArgs {
            key,
            digest: digest.to_vec(),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_SIGN, body)?;
        Ok(wire::decode_sign_reply(&reply.body)
            .map_err(Error::Parcel)?
            .tag)
    }

    /// Seal `plaintext` under the stored key.
    pub fn wrap(&self, key: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
        let body = wire::encode_wrap_args(&wire::WrapArgs {
            key,
            plaintext: plaintext.to_vec(),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_WRAP, body)?;
        Ok(wire::decode_wrap_reply(&reply.body)
            .map_err(Error::Parcel)?
            .blob)
    }

    /// Open a blob produced by [`Client::wrap`] for this key.
    pub fn unwrap(&self, key: u64, blob: &[u8]) -> Result<Vec<u8>> {
        let body = wire::encode_unwrap_args(&wire::UnwrapArgs {
            key,
            blob: blob.to_vec(),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_UNWRAP, body)?;
        Ok(wire::decode_unwrap_reply(&reply.body)
            .map_err(Error::Parcel)?
            .plaintext)
    }

    /// Cryptographically strong bytes.
    pub fn random(&self, len: usize) -> Result<Vec<u8>> {
        let body = wire::encode_random_args(&wire::RandomArgs { len: len as u64 })
            .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_RANDOM, body)?;
        Ok(wire::decode_random_reply(&reply.body)
            .map_err(Error::Parcel)?
            .bytes)
    }

    /// Create a fresh random key of `kind`; returns its id.
    pub fn generate(&self, kind: &str) -> Result<u64> {
        let body = wire::encode_generate_args(&wire::GenerateArgs {
            kind: String::from(kind),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_GENERATE, body)?;
        Ok(wire::decode_generate_reply(&reply.body)
            .map_err(Error::Parcel)?
            .id)
    }

    /// Key ids and last-use counters; never key material.
    pub fn keys(&self) -> Result<Vec<KeyInfo>> {
        let reply = self.call(wire::METHOD_LIST, Vec::new())?;
        Ok(wire::decode_list_reply(&reply.body)
            .map_err(Error::Parcel)?
            .keys)
    }

    /// Round-trip probe.
    pub fn ping(&self) -> Result<()> {
        self.call(wire::METHOD_PING, Vec::new())?;
        Ok(())
    }
}
