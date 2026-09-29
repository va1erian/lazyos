//! Wire shapes for `mimed` (issue #158): MIME/handler registry and app
//! launch records, plus the `system/events/open/<app>` interim launch event.
//! See the module doc on [`crate::messenger::mime`] for the launch path.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{errno, registry, Endpoint, Error, Result};

/// The MIME service's registered name.
pub const NAME: &str = "os.lazy.mimed";

/// `os.lazy.mimed.v1` as an interim eight-byte ABI id (the pattern the
/// other interim service interfaces use).
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.mime.");

/// Methods of the MIME service.
pub mod method {
    /// MIME type for a path, from the database.
    pub const GUESS: u32 = 1;
    /// App registered for a type and verb.
    pub const LOOKUP: u32 = 2;
    /// Verbs registered for a type.
    pub const VERBS: u32 = 3;
    /// Guess, resolve, and publish the launch event.
    pub const OPEN: u32 = 4;
    /// Add or replace an open-with registration.
    pub const REGISTER: u32 = 5;
}

/// Protocol TLV field ids.
pub mod field {
    /// Path to guess.
    pub const PATH: u16 = 1;
    /// MIME type.
    pub const MIME: u16 = 2;
    /// Shell verb (`open`, `edit`, `reveal`, ...).
    pub const VERB: u16 = 3;
    /// App id.
    pub const APP: u16 = 4;
    /// One verb of a `Verbs` reply.
    pub const VERBS: u16 = 5;
    /// Lookup verdict (`1` = an app is registered).
    pub const FOUND: u16 = 6;
    /// Whether the launch event went out.
    pub const PUBLISHED: u16 = 7;
    /// Launch event topic.
    pub const TOPIC: u16 = 8;
    /// Structured error reply.
    pub const ERROR: u16 = 9;
    /// Whether `init` launched the resolved app (issue #158).
    pub const LAUNCHED: u16 = 10;
}

/// Type reported for a path the database has no entry for.
pub const FALLBACK_MIME: &str = "application/octet-stream";

/// Verb [`Client::open`] falls back to when the requested verb has no
/// registration for the guessed type.
pub const DEFAULT_VERB: &str = "open";

/// One `Open` resolution: the app that will handle the file, its type, and
/// the launch event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenResult {
    /// App id from the open-with registry.
    pub app: String,
    /// Type the path guessed to.
    pub mime: String,
    /// Topic the launch event was published on.
    pub topic: String,
    /// Whether the launch event went out.
    pub published: bool,
    /// Whether `init` accepted the launch request for the app (#158).
    pub launched: bool,
}

/// A header for a MIME parcel of `method`.
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

/// Wrap an encoded body in a MIME parcel.
fn parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: header(method),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// A `Guess(path)` request.
pub fn guess_request(path: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::PATH, path).map_err(Error::Parcel)?;
    Ok(parcel(method::GUESS, body))
}

/// A `Lookup(mime, verb)` request.
pub fn lookup_request(mime: &str, verb: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::MIME, mime).map_err(Error::Parcel)?;
    body.string(field::VERB, verb).map_err(Error::Parcel)?;
    Ok(parcel(method::LOOKUP, body))
}

/// A `Verbs(mime)` request.
pub fn verbs_request(mime: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::MIME, mime).map_err(Error::Parcel)?;
    Ok(parcel(method::VERBS, body))
}

/// An `Open(path, verb)` request.
pub fn open_request(path: &str, verb: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::PATH, path).map_err(Error::Parcel)?;
    body.string(field::VERB, verb).map_err(Error::Parcel)?;
    Ok(parcel(method::OPEN, body))
}

/// A `Register(mime, app, verb)` request (the registry takes the latest
/// registration for each type and verb).
pub fn register_request(mime: &str, app: &str, verb: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::MIME, mime).map_err(Error::Parcel)?;
    body.string(field::APP, app).map_err(Error::Parcel)?;
    body.string(field::VERB, verb).map_err(Error::Parcel)?;
    Ok(parcel(method::REGISTER, body))
}

/// A `Guess` reply carrying the type.
pub fn guess_reply(mime: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::MIME, mime).map_err(Error::Parcel)?;
    Ok(parcel(method::GUESS, body))
}

/// A `Lookup` reply: `FOUND`, then the app when one is registered.
pub fn lookup_reply(app: Option<&str>) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::FOUND, app.is_some() as u64)
        .map_err(Error::Parcel)?;
    if let Some(app) = app {
        body.string(field::APP, app).map_err(Error::Parcel)?;
    }
    Ok(parcel(method::LOOKUP, body))
}

/// A `Verbs` reply: one string field per verb.
pub fn verbs_reply(verbs: &[String]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for verb in verbs {
        body.string(field::VERBS, verb).map_err(Error::Parcel)?;
    }
    Ok(parcel(method::VERBS, body))
}

/// An `Open` reply describing the resolution and the launch event.
pub fn open_reply(result: &OpenResult) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::APP, &result.app)
        .map_err(Error::Parcel)?;
    body.string(field::MIME, &result.mime)
        .map_err(Error::Parcel)?;
    body.string(field::TOPIC, &result.topic)
        .map_err(Error::Parcel)?;
    body.u64(field::PUBLISHED, result.published as u64)
        .map_err(Error::Parcel)?;
    body.u64(field::LAUNCHED, result.launched as u64)
        .map_err(Error::Parcel)?;
    Ok(parcel(method::OPEN, body))
}

/// An empty success reply (a `Register`).
pub fn ok_reply(method: u32) -> Parcel {
    parcel(method, Encoder::new())
}

/// The service's error answer: errno-style code plus friendly text. The
/// client turns the code back into [`Error::Mime`].
pub fn error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    parcel(method, body)
}

/// The first structured error field, when the reply is a service failure.
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

/// The first string field with `id`, or a malformed-request error.
pub fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// The first string field with `id`, when present.
fn optional_string(parcel: &Parcel, id: u16) -> Option<String> {
    string_field(parcel, id).ok()
}

/// The first `u64` field with `id`, if any.
fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// Every string field with `id`, in order.
fn string_fields(parcel: &Parcel, id: u16) -> Vec<String> {
    let mut values = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::String && field.id == id {
            if let Ok(text) = field.as_str() {
                values.push(String::from(text));
            }
        }
    }
    values
}

/// Decode a `Guess` reply.
pub fn decode_guess(parcel: &Parcel) -> Result<String> {
    string_field(parcel, field::MIME)
}

/// Decode a `Lookup` reply; `None` when no app is registered.
pub fn decode_lookup(parcel: &Parcel) -> Result<Option<String>> {
    if u64_field(parcel, field::FOUND).unwrap_or(0) == 0 {
        return Ok(None);
    }
    optional_string(parcel, field::APP)
        .map(Some)
        .ok_or(Error::Errno(-errno::EINVAL))
}

/// Decode a `Verbs` reply.
pub fn decode_verbs(parcel: &Parcel) -> Result<Vec<String>> {
    Ok(string_fields(parcel, field::VERBS))
}

/// Decode an `Open` reply.
pub fn decode_open(parcel: &Parcel) -> Result<OpenResult> {
    Ok(OpenResult {
        app: string_field(parcel, field::APP)?,
        mime: optional_string(parcel, field::MIME).unwrap_or_default(),
        topic: optional_string(parcel, field::TOPIC).unwrap_or_default(),
        published: u64_field(parcel, field::PUBLISHED).unwrap_or(0) != 0,
        launched: u64_field(parcel, field::LAUNCHED).unwrap_or(0) != 0,
    })
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
    fn call(&self, request: &Parcel) -> Result<Parcel> {
        let reply = self.endpoint.call(request, None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Mime(code));
        }
        Ok(reply)
    }

    /// MIME type for `path`.
    pub fn guess(&self, path: &str) -> Result<String> {
        let reply = self.call(&guess_request(path)?)?;
        decode_guess(&reply)
    }

    /// App registered for `mime` and `verb`; `None` when none is.
    pub fn lookup(&self, mime: &str, verb: &str) -> Result<Option<String>> {
        let reply = self.call(&lookup_request(mime, verb)?)?;
        decode_lookup(&reply)
    }

    /// Verbs registered for `mime`, in registration order.
    pub fn verbs(&self, mime: &str) -> Result<Vec<String>> {
        let reply = self.call(&verbs_request(mime)?)?;
        decode_verbs(&reply)
    }

    /// Guess `path`, resolve the app for `verb`, and publish the launch
    /// event. [`OpenResult::published`] reports whether the event went out.
    pub fn open(&self, path: &str, verb: &str) -> Result<OpenResult> {
        let reply = self.call(&open_request(path, verb)?)?;
        decode_open(&reply)
    }

    /// Add or replace the app registered for `mime` and `verb`.
    pub fn register(&self, mime: &str, app: &str, verb: &str) -> Result<()> {
        self.call(&register_request(mime, app, verb)?)?;
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
