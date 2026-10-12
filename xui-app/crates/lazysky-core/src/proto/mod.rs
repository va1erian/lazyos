//! The Bluesky wire protocol as the client uses it (docs/lazysky-plan.md
//! §7.3): typed views over XRPC JSON, built by hand from `serde_json::Value`
//! so that one bad field never fails a page.
//!
//! No I/O lives here. Every parser takes server bytes or a `Value`, treats
//! every field as untrusted and returns `None`/skips on a malformed item. A
//! post keeps its raw JSON (`Post::raw`) for the Raw view.

pub mod actors;
pub mod embed;
pub mod error;
pub mod facets;
pub mod feed;
pub mod notifications;
pub mod record;
pub mod session;
pub mod thread;
pub mod time;

use serde_json::Value;

pub use actors::{Actor, Profile};
pub use embed::{Embed, ExternalCard, ImageRef, Quoted, VideoRef};
pub use error::{ErrorKind, XrpcError};
pub use facets::{Facet, Feature};
pub use feed::{FeedItem, Page, Post, Reason, StrongRef, ViewerState};
pub use notifications::{Notification, NotificationKind};
pub use session::Session;
pub use thread::{Thread, ThreadNode};

/// The string at `key`, if there is one.
pub(crate) fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_owned)
}

/// The unsigned number at `key`, or 0 (a count the server left out).
pub(crate) fn count(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The `$type` of an object.
pub(crate) fn type_of(v: &Value) -> &str {
    v.get("$type").and_then(Value::as_str).unwrap_or("")
}

/// Parses a JSON body, or says why not (never echoing the body).
pub fn parse_json(body: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(body).map_err(|e| format!("the server's answer is not JSON: {e}"))
}
