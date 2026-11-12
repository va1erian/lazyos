//! Wire shapes for `mimed` (issue #158): MIME/handler registry and app
//! launch records, plus the `system/events/open/<app>` interim launch event.
//! See the module doc on [`crate::messenger::mime`] for the launch path.

use alloc::string::String;
use alloc::vec::Vec;

use super::client::Native;

/// The generated `os.lazy.mimed.v1` stubs (see `idl/mimed.midl`).
pub use messenger_generated::os_lazy_mimed_v1 as wire;

/// The MIME service's registered name.
pub const NAME: &str = wire::SERVICE_NAME;

/// The interface id every `mimed` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Type reported for a path the database has no entry for.
pub const FALLBACK_MIME: &str = "application/octet-stream";

/// Verb [`Client::open`](wire::Client::open) falls back to when the requested verb has no
/// registration for the guessed type.
pub const DEFAULT_VERB: &str = "open";

/// One `Open` resolution: the app that will handle the file, its type, and
/// the launch event (`launched`: whether `init` accepted the launch, #158).
pub type OpenResult = wire::OpenReply;

/// A blocking client of the `mimed` service: `Client::connect(Native)`.
pub type Client = wire::Client<Native>;

/// Why a `mimed` call failed.
pub type ClientError = wire::ClientError;

/// Convenience: the MIME type for `path`, or [`FALLBACK_MIME`] when the
/// service is unreachable.
pub fn guess(path: &str) -> String {
    Client::connect(Native)
        .and_then(|client| client.guess(path))
        .unwrap_or_else(|_| String::from(FALLBACK_MIME))
}

/// Convenience: the app registered for `mime` and `verb`; `None` when none
/// is registered or the service is unreachable.
pub fn lookup(mime: &str, verb: &str) -> Option<String> {
    Client::connect(Native).ok()?.lookup(mime, verb).ok()?
}

/// Convenience: the verbs registered for `mime` (empty when unreachable).
pub fn verbs(mime: &str) -> Vec<String> {
    Client::connect(Native)
        .and_then(|client| client.verbs(mime))
        .unwrap_or_default()
}

/// Convenience: connect and open.
pub fn open(path: &str, verb: &str) -> Result<OpenResult, ClientError> {
    Client::connect(Native)?.open(path, verb)
}
