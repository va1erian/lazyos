//! File confinement for sandboxed process labels (docs/security-model.md,
//! "Sandboxed processes"; docs/webkit-port-plan.md W5).
//!
//! A manifest's `files = ["read:...", "write:..."]` was consent-only until
//! the kernel gained a file gate: a task whose label is *confined* may touch
//! exactly the paths its label's file rules name, plus a small fixed
//! [`baseline`] (the data devices and the fabricated `/proc`). This crate is
//! the one place that decides what a rule means, so the two halves cannot
//! drift:
//!
//! * [`compile`] turns the manifest's rules into **rule ids**, which the
//!   package manager loads as ordinary allow rules of the label
//!   (`LoadLabel`, interface id = the rule id, any method): no second loader,
//!   no second table, revoking a label drops its files with it;
//! * [`allows`] is what the kernel's VFS asks, with the label's loaded ids
//!   behind a predicate, before it looks at a node.
//!
//! # Rule grammar
//!
//! `read:<path>` or `write:<path>`; the path is absolute or starts with
//! `$HOME/` (the home directory of the user running the app, `/home/<name>`).
//! A rule naming a directory covers everything inside it, so a trailing
//! `/*` is accepted and means exactly the directory; `*` anywhere else, `..`,
//! `.`, empty segments and an absolute path inside a home are refused
//! ([`RuleError`]). `write` includes `read`, and both include `stat`.
//!
//! # What an access means
//!
//! The kernel hands [`allows`] an absolute, lexically normalised path (there
//! are no symlinks, and `..` has been folded before any lookup) and the
//! [`Access`] the operation needs:
//!
//! * [`Access::Write`]: creating, removing, renaming, truncating, writing and
//!   changing attributes; for `create`/`unlink`/`rename` the path is the
//!   *parent directory*, as for the permission bits;
//! * [`Access::Read`]: reading data, listing a directory, executing;
//! * [`Access::Stat`]: existence and metadata. A confined task may also stat
//!   the ancestors of what it was granted, so `realpath` and a directory walk
//!   to a granted file work, and nothing else.
//!
//! # Collisions
//!
//! A rule id is the 64-bit FNV-1a hash of a tag and the rule's path. Ids are
//! derived from the manifest of a package the user approved, never from what a
//! sandboxed task names, so an attacker needs a collision with a path they do
//! not choose; the id space is also disjoint from interface ids by its prefix.
//! The kernel's name and development-label rules have always been keyed by
//! 32-bit hashes the same way.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod check;
mod rule;
#[cfg(test)]
mod tests;

pub use check::{allows, baseline, Access};
pub use rule::{compile, parse, Grant, Rule, RuleError, MAX_PATH, MAX_SEGMENTS};

use alloc::string::String;

/// The part of a rule id that says what it grants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Marks a label as confined: present in every compiled rule set, so a
    /// label that holds file rules is confined whatever it is called.
    Confine,
    /// Read (and stat) the path and everything inside it.
    Read,
    /// Write, read and stat the path and everything inside it.
    Write,
    /// Stat the path itself only: an ancestor of a granted path.
    Trav,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Confine => "confine",
            Kind::Read => "read",
            Kind::Write => "write",
            Kind::Trav => "trav",
        }
    }
}

/// Longest path [`allows`] judges, in bytes (the Linux ABI's `PATH_MAX`);
/// anything longer is refused.
pub const MAX_PATH_BYTES: usize = 4096;

/// The prefix every rule id hashes under.
const DOMAIN: &str = "lazyos.file.";

/// FNV-1a, 64 bit (the hash the kernel keys interface ids and policy by).
const fn fnv_step(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        i += 1;
    }
    hash
}

/// The id of the rule `kind` on `subject` (`/a/b`, or `$HOME/a/b`, or empty
/// for [`Kind::Confine`]).
pub fn rule_id(kind: Kind, subject: &str) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    hash = fnv_step(hash, DOMAIN.as_bytes());
    hash = fnv_step(hash, kind.tag().as_bytes());
    hash = fnv_step(hash, b":");
    fnv_step(hash, subject.as_bytes())
}

/// The id of the marker rule every compiled rule set carries.
pub fn confine_id() -> u64 {
    rule_id(Kind::Confine, "")
}

/// The first segment of a home directory's path (`home` for `/home/<name>`).
fn home_root_segment() -> &'static str {
    fhs::state::HOME_ROOT.trim_matches('/')
}

/// The text `$HOME` stands for in subjects.
pub const HOME_VAR: &str = "$HOME";

/// A subject for a list of segments: `/a/b`, or `/` for none.
pub(crate) fn literal_subject(segments: &[&str]) -> String {
    if segments.is_empty() {
        return String::from("/");
    }
    let mut out = String::new();
    for segment in segments {
        out.push('/');
        out.push_str(segment);
    }
    out
}

/// A subject below `$HOME`: `$HOME`, or `$HOME/a/b`.
pub(crate) fn home_subject(segments: &[&str]) -> String {
    let mut out = String::from(HOME_VAR);
    for segment in segments {
        out.push('/');
        out.push_str(segment);
    }
    out
}
