//! Core packages (issue #509 §3): what `pkgd` does with the packages the image
//! ships in `/system/packages`, decided without a syscall so the host tests pin
//! every case.
//!
//! A package is **core** when its `system_name` is shipped in
//! `/system/packages`; who installed the current version does not matter. At
//! every start, after reconciliation, `pkgd`:
//!
//! 1. learns the shipped set `(system_name, version, digest)` from the image's
//!    index ([`parse_index`]) without opening the archives;
//! 2. compares the [`stamp`] of that set with the one it stored last time: if
//!    they match and every shipped app has a row, nothing changed
//!    ([`up_to_date`]) and the boot costs a listing and one small read;
//! 3. otherwise follows [`plan`]: install what is missing, upgrade what is
//!    older (or the same version built differently), keep a newer version the
//!    user installed, and turn a core row whose app the image dropped into a
//!    user row, which can then be removed.
//!
//! [`removal`] and [`downgrade`] are the two refusals a core app adds to
//! `Remove` and `Install`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use lazypkg::Version;

use crate::layout;

/// The `confd` key holding the stamp of the last provisioned set. Under
/// `sys/`, so only `pkgd` (root) writes it.
pub const STAMP_KEY: &str = "sys/pkgd/provisioned";

/// The longest index `pkgd` reads (one short line per core package).
pub const MAX_INDEX: usize = 16 * 1024;

/// Most core packages one image ships.
pub const MAX_CORE: usize = 64;

/// One package the image ships.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shipped {
    pub system_name: String,
    pub version: String,
    /// Lowercase hex SHA-256 of the archive.
    pub digest: String,
}

/// What is installed for one app (its `confd` row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Current {
    pub system_name: String,
    pub version: String,
    pub digest: String,
    /// The row records the app as core.
    pub core: bool,
}

/// One provisioning step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Not installed: install the shipped package as core.
    Install(String),
    /// Installed at another digest, not newer than the shipped version:
    /// replace it with the shipped package.
    Upgrade(String),
    /// A newer version the user installed over the core app: leave it.
    Keep {
        system_name: String,
        installed: String,
        shipped: String,
    },
    /// Installed at the shipped digest but not recorded as core.
    MarkCore(String),
    /// Recorded as core, but the image no longer ships it: it stays installed
    /// as a user app (and becomes removable).
    Demote(String),
}

/// What one provisioning pass did, for `PKGD:PROVISION:DONE`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub installed: u64,
    pub upgraded: u64,
    pub kept: u64,
    pub failed: u64,
}

impl Tally {
    /// The serial marker for a finished pass.
    pub fn done_line(&self) -> String {
        format!(
            "PKGD:PROVISION:DONE installed={} upgraded={} kept={} failed={}\n",
            self.installed, self.upgraded, self.kept, self.failed
        )
    }
}

/// The stamp of a shipped set: SHA-256 over its sorted `(system_name,
/// digest)` pairs, in hex. The order of the input does not matter.
pub fn stamp(shipped: &[Shipped]) -> String {
    let mut pairs: Vec<(&str, &str)> = shipped
        .iter()
        .map(|p| (p.system_name.as_str(), p.digest.as_str()))
        .collect();
    pairs.sort_unstable();
    let mut text = String::new();
    for (name, digest) in pairs {
        text.push_str(name);
        text.push(' ');
        text.push_str(digest);
        text.push('\n');
    }
    lazyos_crypto::hex::encode(&lazyos_crypto::sha256::sha256(text.as_bytes()))
}

/// Whether the last pass already provisioned exactly this set: the stored
/// stamp matches and every shipped app still has a row recorded as core.
pub fn up_to_date(stored: Option<&str>, shipped: &[Shipped], current: &[Current]) -> bool {
    stored == Some(stamp(shipped).as_str())
        && shipped.iter().all(|package| {
            current
                .iter()
                .any(|row| row.system_name == package.system_name && row.core)
        })
}

/// Whether `installed` is strictly newer than `shipped`. A version that does
/// not parse is never newer, so a damaged row is replaced, not kept.
fn newer(installed: &str, shipped: &str) -> bool {
    match (Version::parse(installed), Version::parse(shipped)) {
        (Ok(installed), Ok(shipped)) => installed > shipped,
        _ => false,
    }
}

/// The steps that bring `current` in line with `shipped`, in `system_name`
/// order (shipped apps first, then the rows to demote).
pub fn plan(shipped: &[Shipped], current: &[Current]) -> Vec<Action> {
    let mut packages: Vec<&Shipped> = shipped.iter().collect();
    packages.sort_by(|a, b| a.system_name.cmp(&b.system_name));
    let mut actions = Vec::new();
    for package in packages {
        let name = package.system_name.clone();
        let row = current.iter().find(|row| row.system_name == name);
        actions.push(match row {
            None => Action::Install(name),
            Some(row) if row.digest == package.digest => {
                if row.core {
                    continue;
                }
                Action::MarkCore(name)
            }
            Some(row) if newer(&row.version, &package.version) => Action::Keep {
                system_name: name,
                installed: row.version.clone(),
                shipped: package.version.clone(),
            },
            Some(_) => Action::Upgrade(name),
        });
    }
    let mut dropped: Vec<&Current> = current
        .iter()
        .filter(|row| row.core && !shipped.iter().any(|p| p.system_name == row.system_name))
        .collect();
    dropped.sort_by(|a, b| a.system_name.cmp(&b.system_name));
    actions.extend(
        dropped
            .into_iter()
            .map(|row| Action::Demote(row.system_name.clone())),
    );
    actions
}

/// Put the installs and upgrades largest package first (`size` is the
/// archive's size in bytes), the other steps after them. Extraction reuses
/// one buffer sized to the largest file it has seen, and `pkgd`'s heap never
/// returns a block over 64 KiB, so meeting the big packages first means the
/// buffer grows once instead of once per bigger package.
pub fn largest_first(actions: &mut [Action], size: impl Fn(&str) -> usize) {
    actions.sort_by_key(|action| match action {
        Action::Install(name) | Action::Upgrade(name) => core::cmp::Reverse(size(name)),
        _ => core::cmp::Reverse(0),
    });
}

/// Why the index is unusable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexError {
    pub line: usize,
    pub reason: &'static str,
}

/// Parse the image's core package index: one `<system_name> <version>
/// <sha256 hex>` line per package, `#` comments and blank lines ignored. The
/// file is on `/system`, which only an image update writes, but it is still
/// validated: a bad line fails the whole index, and `pkgd` then reads the
/// archives themselves.
pub fn parse_index(text: &str) -> Result<Vec<Shipped>, IndexError> {
    let mut out: Vec<Shipped> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |reason| IndexError {
            line: number + 1,
            reason,
        };
        let mut words = line.split_ascii_whitespace();
        let (Some(name), Some(version), Some(digest), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(bad("expected `<system_name> <version> <digest>`"));
        };
        if !layout::valid_system_name(name) {
            return Err(bad("malformed system_name"));
        }
        if Version::parse(version).is_err() {
            return Err(bad("malformed version"));
        }
        if !valid_digest(digest) {
            return Err(bad("the digest is not 64 lowercase hex digits"));
        }
        if out.iter().any(|p| p.system_name == name) {
            return Err(bad("a system_name is listed twice"));
        }
        if out.len() == MAX_CORE {
            return Err(bad("too many packages"));
        }
        out.push(Shipped {
            system_name: name.to_string(),
            version: version.to_string(),
            digest: digest.to_string(),
        });
    }
    Ok(out)
}

/// Format an index [`parse_index`] reads back (the image build writes the same
/// format from Python).
pub fn format_index(shipped: &[Shipped]) -> String {
    let mut text = String::new();
    for package in shipped {
        text.push_str(&format!(
            "{} {} {}\n",
            package.system_name, package.version, package.digest
        ));
    }
    text
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The system_name a `/system/packages` file name stands for:
/// `<system_name>.lzp`, or `None` for anything else.
pub fn package_file_name(file: &str) -> Option<&str> {
    let name = file.strip_suffix(".lzp")?;
    layout::valid_system_name(name).then_some(name)
}

/// `Remove`'s refusal for a core app, whoever asks (root included: the core
/// set changes with image updates, not with `pkgctl remove`). `name` is the
/// display name.
pub fn removal(name: &str, core: bool) -> Result<(), String> {
    if core {
        Err(format!(
            "{name} is part of LazyOS and can't be removed; you can hide it from the menu in Settings"
        ))
    } else {
        Ok(())
    }
}

/// `Install`'s refusal of a version of a core app older than the one the image
/// ships. A version that does not parse is refused too (the package validator
/// already rejects it, so this never decides alone).
pub fn downgrade(name: &str, shipped: &str, incoming: &str) -> Result<(), String> {
    match (Version::parse(shipped), Version::parse(incoming)) {
        (Ok(shipped_v), Ok(incoming_v)) if incoming_v >= shipped_v => Ok(()),
        _ => Err(format!(
            "{name} {incoming} is older than the built-in version {shipped}"
        )),
    }
}

#[cfg(test)]
mod tests;
