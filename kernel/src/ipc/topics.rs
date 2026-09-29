//! Topic ACL hooks for the userspace pub/sub broker (issue #92).
//!
//! `docs/messenger.md` section 20 records the epic decision: topics live in
//! userspace first. `messengerd` owns hierarchical names, `+`/`#` filter
//! matching, QoS queues, retained values and fanout; the kernel owns exactly
//! one thing — policy. This module is the minimum kernel surface that split
//! needs: for every segment of a topic (or of a subscription filter) the
//! broker asks [`crate::ipc::authorize`] with a stable `(interface, method)`
//! pair, so an administrator can allow or deny whole namespaces per actor
//! without the broker owning policy.
//!
//! * Publish and subscribe are separate pseudo-interfaces, so policy can grant
//!   `system/events` reads to every app while restricting writes to `netd`.
//! * The method id is `fnv1a32` of one segment, the same hash `tools/midlc`
//!   uses for method names. `+` and `#` hash like any other segment, so a
//!   wildcard subscribe can be denied explicitly instead of bypassing the
//!   per-segment rules.
//! * Evaluation is per segment and default-deny once a policy is loaded, and
//!   every verdict goes through the audited [`crate::ipc::authorize`] hook, so
//!   a denied publish shows up in the audit ring with its correlation id.
//!
//! The broker reaches this module through the `authorize_topic` syscall op;
//! the syscall layer only validates pointers and the proxy capability (a
//! `messengerd` request carries the *client's* slot), never policy itself.

/// FNV-1a 64, the interface-id hash from `tools/midlc` (`fnv1a64`).
pub const fn fnv1a64(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0000_0100_0000_01B3);
        index += 1;
    }
    hash
}

/// FNV-1a 32 (31-bit positive), the method-id hash from `tools/midlc`.
pub const fn fnv1a32(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut hash = 0x811C_9DC5u32;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u32).wrapping_mul(0x0100_0193);
        index += 1;
    }
    hash & 0x7FFF_FFFF
}

/// Policy interface used for `Publish` authorization.
///
/// `fnv1a64("os.lazy.messenger.topics.publish.v1")`; the name mirrors the
/// interface convention so a policy compiler can derive it from the IDL-style
/// name.
pub const PUBLISH_INTERFACE: u64 = 0x7ffc_19b0_3e94_1e16;
/// Policy interface used for `Subscribe`/`Unsubscribe` authorization:
/// `fnv1a64("os.lazy.messenger.topics.subscribe.v1")`.
pub const SUBSCRIBE_INTERFACE: u64 = 0xefbc_15f1_4c9d_4bef;

/// Publish-mode code shared with `user/src/messenger/`.
pub const MODE_PUBLISH: u32 = 0;
/// Subscribe-mode code shared with `user/src/messenger/`.
pub const MODE_SUBSCRIBE: u32 = 1;

/// Longest topic/filter accepted by the ACL gate, mirroring the registry's
/// name limit.
pub const MAX_NAME_BYTES: usize = 128;
/// Deepest topic accepted by the ACL gate, mirroring the broker's limit.
pub const MAX_SEGMENTS: usize = 8;

/// TLV field ids of the `authorize_topic` request parcel, mirrored by
/// `user/src/messenger/` (`topics::auth_field`).
pub mod field {
    /// The topic or filter name.
    pub const NAME: u16 = 1;
    /// The mode code ([`MODE_PUBLISH`] or [`MODE_SUBSCRIBE`]).
    pub const MODE: u16 = 2;
    /// Optional audit correlation id (the broker request's transaction).
    pub const TXN: u16 = 3;
}

/// Why [`authorize`] refused to even evaluate a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A segment (or a wildcard placement) is not a valid topic/filter.
    BadName,
    /// The mode code is not [`MODE_PUBLISH`] or [`MODE_SUBSCRIBE`].
    BadMode,
    /// Policy refused at least one segment; audited by `authorize`.
    Denied,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::BadName => "the topic or filter is malformed",
            Error::BadMode => "the topic authorization mode is unknown",
            Error::Denied => "this app is not allowed to use that topic segment",
        }
    }
}

/// The policy interface id for `mode`, or `None` when the code is unknown.
pub fn interface(mode: u32) -> Option<u64> {
    match mode {
        MODE_PUBLISH => Some(PUBLISH_INTERFACE),
        MODE_SUBSCRIBE => Some(SUBSCRIBE_INTERFACE),
        _ => None,
    }
}

/// The policy method id for one literal segment.
pub fn segment_method(segment: &str) -> u32 {
    fnv1a32(segment)
}

/// Whether `byte` may appear inside a topic segment. Lower-case names are the
/// documented style; upper-case is accepted so machine-generated topics (e.g.
/// D-Bus-style interface names) stay usable.
fn valid_segment_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
}

/// Validate a topic (`mode == PUBLISH`) or filter (`mode == SUBSCRIBE`).
///
/// Returns the segment count. Publish topics are literal: wildcards are
/// refused. Filters may use `+` for exactly one segment and `#` for the
/// (possibly empty) tail; `#` anywhere but last is refused, so a filter always
/// has a literal prefix the policy can key on.
pub fn validate(name: &str, mode: u32) -> Result<usize, Error> {
    if interface(mode).is_none() {
        return Err(Error::BadMode);
    }
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(Error::BadName);
    }
    let mut segments = 0usize;
    let mut parts = name.split('/').peekable();
    while let Some(segment) = parts.next() {
        let last = parts.peek().is_none();
        if segment.is_empty() || segment.len() > MAX_NAME_BYTES {
            return Err(Error::BadName);
        }
        if !segment.bytes().all(valid_segment_byte) {
            return Err(Error::BadName);
        }
        if segment.as_bytes().contains(&b'+') && mode != MODE_SUBSCRIBE {
            return Err(Error::BadName);
        }
        if segment.as_bytes().contains(&b'#') && (mode != MODE_SUBSCRIBE || !last) {
            return Err(Error::BadName);
        }
        segments += 1;
        if segments > MAX_SEGMENTS {
            return Err(Error::BadName);
        }
    }
    Ok(segments)
}

/// Authorize every segment of `name` for the actor in `actor_slot`.
///
/// The whole name is checked before anything is allowed: the first denied
/// segment short-circuits with [`Error::Denied`] (and its audit record already
/// written by [`crate::ipc::authorize`]). `txn_id` is copied into every audit
/// record so a denial can be correlated with the broker request that caused
/// it.
pub fn authorize(actor_slot: usize, mode: u32, name: &str, txn_id: u64) -> Result<usize, Error> {
    let interface = interface(mode).ok_or(Error::BadMode)?;
    let segments = validate(name, mode)?;
    for segment in name.split('/') {
        let decision =
            crate::ipc::authorize(actor_slot, interface, segment_method(segment), txn_id);
        if decision.denied() {
            return Err(Error::Denied);
        }
    }
    Ok(segments)
}
