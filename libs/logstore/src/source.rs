//! Which journal a record goes to.
//!
//! A topic is chosen by whoever published it, so it is never trusted as a
//! path component: the source segment is used as a file name only when it is
//! `[a-z0-9_-]{1,32}` and not reserved, and everything else lands in
//! [`SYSTEM`].

/// The journal for records without a usable source segment.
pub const SYSTEM: &str = "system";
/// The journal for the fabric's denial samples.
pub const KERNEL: &str = "kernel";
/// The topic `logd` appends its fabric denial samples on.
pub const DENIAL_TOPIC: &str = "system/events/security/denial";
/// Longest source name.
pub const MAX_SOURCE: usize = 32;
/// Sources whose journal another service owns: `pkgd` writes and caps
/// `pkg.log` itself, so `logd` never writes, rotates or counts it.
pub const RESERVED: &[&str] = &["pkg"];

/// Whether `name` may name a journal file: `[a-z0-9_-]{1,32}`. A reserved
/// source is valid as a name (it can be listed) but never written by `logd`;
/// see [`source_of`].
pub fn valid_source(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SOURCE
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Whether `logd` owns the journal of `name` (writes, rotates and budgets it).
pub fn owned_source(name: &str) -> bool {
    valid_source(name) && !RESERVED.contains(&name)
}

/// The journal for a record on `topic`:
///
/// * the denial samples go to [`KERNEL`];
/// * `system/events/<source>/...` and `system/health/<source>` go to
///   `<source>` when it is a valid, unreserved name;
/// * everything else goes to [`SYSTEM`].
pub fn source_of(topic: &str) -> &str {
    if topic == DENIAL_TOPIC {
        return KERNEL;
    }
    let segment = if let Some(rest) = topic.strip_prefix("system/events/") {
        // At least one segment after the source: `system/events/<source>/x`.
        match rest.split_once('/') {
            Some((source, tail)) if !tail.is_empty() => source,
            _ => return SYSTEM,
        }
    } else if let Some(rest) = topic.strip_prefix("system/health/") {
        rest
    } else {
        return SYSTEM;
    };
    if owned_source(segment) {
        segment
    } else {
        SYSTEM
    }
}
