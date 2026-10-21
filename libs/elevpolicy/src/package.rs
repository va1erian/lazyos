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
//! A developer-tool package (`pkg.install-devtool`) is asked about with
//! [`tool_summary`], which leads with [`devtool::CONSENT_TEXT`] (a shell
//! tool's [`devtool::SHELL_CONSENT_TEXT`]), the one text that says what a tool
//! can do. The prompt has room for [`crate::MAX_SUMMARY`] characters, so what
//! follows the warning is the identity of the package, that it is installed
//! for all users and whether it declares `network` or `develop` (powers that
//! lift denies of the class, so an update that adds one is shown to the
//! administrator), then its author (unverified) when there is room; the
//! rest of its permissions are on the Installer's consent screen.
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

/// What a tool's package may take of the characters after the warning:
/// the app's name, its version and its `system_name`. Together with the
/// warning, the fixed words of the install line (the longest verb, "shell
/// tool", the quotes and brackets, "for all users", and "with network and
/// develop") and its full stop they fit [`crate::MAX_SUMMARY`] with the
/// longest warning, so the install line is trimmed and the warning never is
/// (`devtool_tests` pins the room).
const TOOL_NAME_SHOWN: usize = 20;
/// The fewest characters of the name the line keeps when it is crowded.
const TOOL_NAME_MIN: usize = 8;
const TOOL_VERSION_SHOWN: usize = 8;
const TOOL_SYSTEM_SHOWN: usize = 20;
/// `; author "` and `".` around the author's name.
const AUTHOR_FRAME: usize = 12;
/// The shortest author name worth showing (an ellipsis alone says nothing).
const AUTHOR_MIN: usize = 8;
/// The `kind` of the inspection rows that carry the class, the one of
/// its values that says the tool is the user's shell (`pkgstore::explain::tool`),
/// and the kinds of the manifest's own rows that lift a deny-list entry.
const KIND_ROW: &str = "kind";
const NETWORK_ROW: &str = "network";
const DEVELOP_ROW: &str = "develop";
/// The one `network` value that lifts the socket, name-lookup and mount denies.
const OUTBOUND: &str = "outbound";

/// Whether the inspection's rows name the tool as the user's shell: a second
/// `kind` row, beside the one that names the class.
fn is_shell(permissions: &[Permission]) -> bool {
    permissions
        .iter()
        .any(|row| row.kind == KIND_ROW && row.value == devtool::SHELL_ROW_VALUE)
}

/// Whether the package declares `network = ["outbound"]`, which lifts the
/// three network denies of the class (`devtool::DenyWhen::UnlessNetwork`).
fn has_network(permissions: &[Permission]) -> bool {
    permissions
        .iter()
        .any(|row| row.kind == NETWORK_ROW && row.value == OUTBOUND)
}

/// Whether the package declares `develop = true`, which lifts the dev-spawn
/// deny (`devtool::DenyWhen::UnlessDevelop`).
fn has_develop(permissions: &[Permission]) -> bool {
    permissions.iter().any(|row| row.kind == DEVELOP_ROW)
}

/// The prompt's summary of installing a developer tool: the warning first
/// (a shell tool's own, [`devtool::SHELL_CONSENT_TEXT`], because the generic
/// one says the credential services are refused), then which package
/// (`Update` for a core developer tool it replaces, else `Install`; a shell
/// tool is called one, because it is judged as the user), that it is
/// installed for all users (the stored row is machine-wide, and its autostart
/// opens in every session) and what it declares beyond the class: "with
/// network" and "with develop" lift denies the class otherwise holds, so an
/// approved update that adds one is shown, not silent. The author
/// (unverified) follows when it still fits; the warning is never trimmed, the
/// install line is. At most [`crate::MAX_SUMMARY`] characters.
pub fn tool_summary(facts: &Facts) -> String {
    // An update of a core developer tool (`replaces`) says so in one word.
    let verb = if facts.replaces.is_some() {
        "Update"
    } else {
        "Install"
    };
    let shell = is_shell(&facts.permissions);
    let kind = if shell { " shell tool" } else { "" };
    let powers = match (
        has_network(&facts.permissions),
        has_develop(&facts.permissions),
    ) {
        (true, true) => " with network and develop",
        (true, false) => " with network",
        (false, true) => " with develop",
        (false, false) => "",
    };
    // The name is the most expendable part of the line: show up to
    // `TOOL_NAME_SHOWN` characters of it, fewer when the longest wording
    // (an update of a shell tool declaring both powers) would not fit.
    let line_with = |name_width: usize| {
        format!(
            "{} {verb}{kind} \"{}\" {} ({}) for all users{powers}",
            devtool::consent_text(shell),
            elide(&facts.name, name_width),
            elide(&facts.version, TOOL_VERSION_SHOWN),
            elide(&facts.system_name, TOOL_SYSTEM_SHOWN)
        )
    };
    let mut width = TOOL_NAME_SHOWN;
    let mut line = line_with(width);
    while line.chars().count() >= crate::MAX_SUMMARY && width > TOOL_NAME_MIN {
        width -= 1;
        line = line_with(width);
    }
    // One more character is the full stop that ends the line without an author.
    let room = crate::MAX_SUMMARY.saturating_sub(line.chars().count());
    if room >= AUTHOR_FRAME + AUTHOR_MIN && !facts.author.is_empty() {
        let author = elide(&facts.author, room - AUTHOR_FRAME);
        return format!("{line}; author \"{author}\".");
    }
    format!("{line}.")
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
