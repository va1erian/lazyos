//! Wire shapes for `mimed` (issue #158): MIME/handler registry and app
//! launch records, plus the `system/events/open/<app>` interim launch event.
//! See the module doc on [`crate::messenger::mime`] for the launch path.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{errno, registry, Endpoint, Error, Result};

/// The generated `os.lazy.mimed.v1` stubs (see `idl/mimed.midl`).
pub use messenger_generated::os_lazy_mimed_v1 as wire;

/// The MIME service's registered name.
pub const NAME: &str = "os.lazy.mimed";

/// The interface id every `mimed` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Structured error field id in a reply body. The generated fields of every
/// reply use small ids (at most five), so this can never collide with a
/// success payload.
const ERROR_FIELD: u16 = 15;

/// Type reported for a path the database has no entry for.
pub const FALLBACK_MIME: &str = "application/octet-stream";

/// Verb [`Client::open`] falls back to when the requested verb has no
/// registration for the guessed type.
pub const DEFAULT_VERB: &str = "open";

/// One `Open` resolution: the app that will handle the file, its type, and
/// the launch event (`launched`: whether `init` accepted the launch, #158).
pub type OpenResult = wire::OpenReply;

/// A parcel of `method` carrying an already-encoded `body`; also the reply
/// builder for the service.
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
        ..Parcel::default()
    }
}

/// An empty success reply (a `Register`).
pub fn ok_reply(method: u32) -> Parcel {
    parcel(method, Vec::new())
}

/// The service's error answer: errno-style code plus friendly text. The
/// client turns the code back into [`Error::Mime`].
pub fn error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(ERROR_FIELD, code as u32, error.message());
    parcel(method, body.finish())
}

/// The first structured error field, when the reply is a service failure.
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

/// A client of the `mimed` service.
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

    /// Run one request as a blocking call and fail on a service error.
    fn call(&self, method: u32, body: Vec<u8>) -> Result<Parcel> {
        let reply = self.endpoint.call(&parcel(method, body), None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Mime(code));
        }
        Ok(reply)
    }

    /// MIME type for `path`.
    pub fn guess(&self, path: &str) -> Result<String> {
        let body = wire::encode_guess_args(&wire::GuessArgs {
            path: String::from(path),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_GUESS, body)?;
        Ok(wire::decode_guess_reply(&reply.body)
            .map_err(Error::Parcel)?
            .mime)
    }

    /// App registered for `mime` and `verb`; `None` when none is.
    pub fn lookup(&self, mime: &str, verb: &str) -> Result<Option<String>> {
        let body = wire::encode_lookup_args(&wire::LookupArgs {
            mime: String::from(mime),
            verb: String::from(verb),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_LOOKUP, body)?;
        Ok(wire::decode_lookup_reply(&reply.body)
            .map_err(Error::Parcel)?
            .app)
    }

    /// Verbs registered for `mime`, in registration order.
    pub fn verbs(&self, mime: &str) -> Result<Vec<String>> {
        let body = wire::encode_verbs_args(&wire::VerbsArgs {
            mime: String::from(mime),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_VERBS, body)?;
        Ok(wire::decode_verbs_reply(&reply.body)
            .map_err(Error::Parcel)?
            .verbs)
    }

    /// Guess `path`, resolve the app for `verb`, and publish the launch
    /// event. [`OpenResult::published`] reports whether the event went out.
    pub fn open(&self, path: &str, verb: &str) -> Result<OpenResult> {
        let body = wire::encode_open_args(&wire::OpenArgs {
            path: String::from(path),
            verb: String::from(verb),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(wire::METHOD_OPEN, body)?;
        wire::decode_open_reply(&reply.body).map_err(Error::Parcel)
    }

    /// Add or replace the app registered for `mime` and `verb`.
    pub fn register(&self, mime: &str, app: &str, verb: &str) -> Result<()> {
        let body = wire::encode_register_args(&wire::RegisterArgs {
            mime: String::from(mime),
            app: String::from(app),
            verb: String::from(verb),
        })
        .map_err(Error::Parcel)?;
        self.call(wire::METHOD_REGISTER, body)?;
        Ok(())
    }
}

/// Convenience: the MIME type for `path`, or [`FALLBACK_MIME`] when the
/// service is unreachable.
pub fn guess(path: &str) -> String {
    match Client::connect().and_then(|client| client.guess(path)) {
        Ok(mime) => mime,
        Err(_) => String::from(FALLBACK_MIME),
    }
}

/// Convenience: the app registered for `mime` and `verb`; `None` when none
/// is registered or the service is unreachable.
pub fn lookup(mime: &str, verb: &str) -> Option<String> {
    Client::connect().ok()?.lookup(mime, verb).ok()?
}

/// Convenience: the verbs registered for `mime` (empty when unreachable).
pub fn verbs(mime: &str) -> Vec<String> {
    Client::connect()
        .and_then(|client| client.verbs(mime))
        .unwrap_or_default()
}

/// Convenience: connect and open.
pub fn open(path: &str, verb: &str) -> Result<OpenResult> {
    Client::connect()?.open(path, verb)
}

/// Convenience: connect and register.
pub fn register(mime: &str, app: &str, verb: &str) -> Result<()> {
    Client::connect()?.register(mime, app, verb)
}
