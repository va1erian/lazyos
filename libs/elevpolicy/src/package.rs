//! What the prompt says about a package (review of #659).
//!
//! `pkg.install` and `pkg.update-core` name a file, but an administrator
//! approves a *package*: `elevd` has `pkgd` inspect the file first and the
//! prompt shows what [`summary`] makes of that inspection: the app's name,
//! system name and version, which core app it replaces and from which
//! version, its author (unverified: packages are not signed), and the
//! permissions it asks for, grouped by risk. Everything in it comes from
//! the package, so every field is escaped and bounded ([`crate::text`]),
//! and the whole fits [`crate::MAX_SUMMARY`].
//!
//! `pkgd` reported the archive's SHA-256 with the inspection; `elevd` hands
//! it back with the install (`InstallApproved`), and `pkgd` installs only
//! bytes with that digest, so the file cannot be swapped between the
//! approval and the install.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::text::elide;

/// One requested permission, as `pkgd`'s inspection lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    /// `interface`, `topic`, `file`, `network`, `develop` or `resident`.
    pub kind: String,
    pub value: String,
    /// `low`, `medium` or `high`.
    pub risk: String,
}

/// What the prompt shows about a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub name: String,
    pub system_name: String,
    pub version: String,
    pub author: String,
    pub permissions: Vec<Permission>,
    /// `Some(installed version)` when it replaces a core app
    /// (`pkg.update-core`).
    pub replaces: Option<String>,
}

/// The prompt's summary of installing (or, with `replaces`, of replacing a
/// core app with) the package. At most [`crate::MAX_SUMMARY`] characters.
pub fn summary(facts: &Facts) -> String {
    let name = elide(&facts.name, 28);
    let system_name = elide(&facts.system_name, 40);
    let version = elide(&facts.version, 16);
    let author = elide(&facts.author, 24);
    let what = match &facts.replaces {
        Some(from) => format!(
            "Replace the core app \"{name}\" ({system_name}) {} with {version}",
            elide(from, 16)
        ),
        None => format!("Install \"{name}\" {version} ({system_name}) for all users"),
    };
    format!(
        "{what}; author \"{author}\" (unverified); {}",
        permissions(&facts.permissions)
    )
}

/// The permissions grouped by risk, the riskiest first: up to three high
/// risk ones by name, the medium and low ones counted.
fn permissions(list: &[Permission]) -> String {
    if list.is_empty() {
        return String::from("asks for no permissions");
    }
    let high: Vec<String> = list
        .iter()
        .filter(|p| p.risk == "high")
        .map(|p| elide(&permission(p), 20))
        .collect();
    let medium = list.iter().filter(|p| p.risk == "medium").count();
    let low = list.len() - high.len() - medium;
    let mut parts: Vec<String> = Vec::new();
    if !high.is_empty() {
        let mut text = high[..high.len().min(3)].join(", ");
        if high.len() > 3 {
            text.push_str(&format!(" +{} more", high.len() - 3));
        }
        parts.push(format!("high risk: {text}"));
    }
    if medium > 0 {
        parts.push(format!("medium: {medium}"));
    }
    if low > 0 {
        parts.push(format!("low: {low}"));
    }
    format!("asks for {}", parts.join("; "))
}

/// One permission in a few words: an interface by its name, the rest as
/// kind and value.
fn permission(p: &Permission) -> String {
    match p.kind.as_str() {
        "interface" => p.value.clone(),
        "develop" | "resident" => p.kind.clone(),
        _ if p.value.is_empty() => p.kind.clone(),
        _ => format!("{} {}", p.kind, p.value),
    }
}
