//! Package versions: dotted numbers with an optional pre-release, ordered like
//! semver.
//!
//! The grammar (`docs/packages.md`, "Versions"; `tools/pkg/pkgmanifest.py`
//! implements the same one):
//!
//! ```text
//! version  = core [ "-" pre ]
//! core     = number ( "." number ){1,3}        two to four components
//! number   = "0" | [1-9][0-9]*                 below 65536, no leading zero
//! pre      = ident ( "." ident )*
//! ident    = [0-9A-Za-z-]+                     an all-digit ident has no leading zero
//! ```
//!
//! at most [`MAX_VERSION_LEN`] bytes. There is no `+build` part.
//!
//! **Ordering.** Cores compare component by component, a missing component
//! counting as `0`, so `1.10 > 1.9` and **`1.0 == 1.0.0`**: two spellings of
//! the same release are the same version (a package that renames `1.0` to
//! `1.0.0` is neither an upgrade nor a downgrade). For equal cores a
//! pre-release sorts before the release (`1.0.0-rc1 < 1.0.0`), and two
//! pre-releases compare identifier by identifier as semver §11 says: numeric
//! identifiers numerically, others in ASCII order, numeric before
//! alphanumeric, and a shorter list before a longer one it prefixes.
//!
//! Equality is that ordering's: `Version::parse("1.0") == Version::parse("1.0.0")`
//! although [`Version::as_str`] still returns the text as written (which is what
//! names the install directory).

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;

/// Longest version string, in bytes.
pub const MAX_VERSION_LEN: usize = 64;
/// Fewest and most numeric components of the core.
const MIN_COMPONENTS: usize = 2;
const MAX_COMPONENTS: usize = 4;

/// A parsed, validated package version.
#[derive(Clone, Debug)]
pub struct Version {
    text: String,
    /// The core, padded with zeros to [`MAX_COMPONENTS`].
    core: [u16; MAX_COMPONENTS],
    /// Byte offset of the pre-release in `text` (after the `-`), if any.
    pre_start: Option<usize>,
}

/// Why a string is not a [`Version`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionError {
    /// Empty, or longer than [`MAX_VERSION_LEN`] bytes.
    Length,
    /// Fewer than two or more than four numeric components.
    Components,
    /// A component that is empty, not decimal, has a leading zero, or is
    /// 65536 or more.
    Number,
    /// An empty pre-release identifier, a byte outside `[0-9A-Za-z-]`, or a
    /// numeric identifier with a leading zero.
    Prerelease,
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            VersionError::Length => "is empty or longer than 64 bytes",
            VersionError::Components => "must have two to four numbers",
            VersionError::Number => {
                "has a number that is not decimal below 65536 without a leading zero"
            }
            VersionError::Prerelease => {
                "has a pre-release part that is not dot-separated [0-9A-Za-z-] identifiers"
            }
        })
    }
}

impl Version {
    /// Parse and validate `text`.
    pub fn parse(text: &str) -> Result<Version, VersionError> {
        if text.is_empty() || text.len() > MAX_VERSION_LEN {
            return Err(VersionError::Length);
        }
        let (core_text, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let core = parse_core(core_text)?;
        if let Some(pre) = pre {
            if !pre.split('.').all(valid_identifier) {
                return Err(VersionError::Prerelease);
            }
        }
        Ok(Version {
            text: String::from(text),
            core,
            pre_start: pre.map(|_| core_text.len() + 1),
        })
    }

    /// The version exactly as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The numeric core, padded with zeros to four components.
    pub fn core(&self) -> [u16; MAX_COMPONENTS] {
        self.core
    }

    /// The pre-release part without its `-`, if any.
    pub fn prerelease(&self) -> Option<&str> {
        self.pre_start.map(|start| &self.text[start..])
    }

    /// Whether this is a pre-release (`1.0.0-rc1`).
    pub fn is_prerelease(&self) -> bool {
        self.pre_start.is_some()
    }
}

fn parse_core(text: &str) -> Result<[u16; MAX_COMPONENTS], VersionError> {
    let parts: Vec<&str> = text.split('.').collect();
    if !(MIN_COMPONENTS..=MAX_COMPONENTS).contains(&parts.len()) {
        return Err(VersionError::Components);
    }
    let mut core = [0u16; MAX_COMPONENTS];
    for (slot, part) in core.iter_mut().zip(parts) {
        if !is_canonical_number(part) {
            return Err(VersionError::Number);
        }
        *slot = part.parse::<u16>().map_err(|_| VersionError::Number)?;
    }
    Ok(core)
}

/// Decimal digits with no leading zero (`0` itself is fine).
fn is_canonical_number(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'))
}

fn valid_identifier(ident: &str) -> bool {
    if ident.is_empty()
        || !ident
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return false;
    }
    !is_numeric(ident) || is_canonical_number(ident)
}

fn is_numeric(ident: &str) -> bool {
    ident.bytes().all(|b| b.is_ascii_digit())
}

/// Semver §11.4: one pre-release identifier against another.
fn compare_identifier(left: &str, right: &str) -> Ordering {
    match (is_numeric(left), is_numeric(right)) {
        // Canonical numbers: a longer one is larger, equal lengths compare as text.
        (true, true) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => left.cmp(right),
    }
}

fn compare_prerelease(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            let mut lefts = left.split('.');
            let mut rights = right.split('.');
            loop {
                match (lefts.next(), rights.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(l), Some(r)) => match compare_identifier(l, r) {
                        Ordering::Equal => {}
                        other => return other,
                    },
                }
            }
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| compare_prerelease(self.prerelease(), other.prerelease()))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Version) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl core::str::FromStr for Version {
    type Err = VersionError;

    fn from_str(text: &str) -> Result<Version, VersionError> {
        Version::parse(text)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|error| panic!("{text:?}: {error}"))
    }

    #[test]
    fn numbers_compare_numerically_not_as_text() {
        assert!(v("1.10") > v("1.9"));
        assert!(v("1.0.10") > v("1.0.9"));
        assert!(v("2.0") > v("1.65535.65535.65535"));
    }

    #[test]
    fn a_missing_component_is_zero() {
        assert_eq!(v("1.0"), v("1.0.0"));
        assert_eq!(v("1.0"), v("1.0.0.0"));
        assert!(v("1.0.0.1") > v("1.0"));
        assert_eq!(v("1.0").as_str(), "1.0");
    }

    #[test]
    fn a_prerelease_precedes_its_release() {
        assert!(v("1.0.0-rc1") < v("1.0.0"));
        assert!(v("1.0.0-rc1") > v("0.9.9"));
        assert_eq!(v("1.0-rc1"), v("1.0.0-rc1"));
        assert_eq!(v("1.0.0-rc1").prerelease(), Some("rc1"));
        assert!(!v("1.0.0").is_prerelease());
    }

    #[test]
    fn prereleases_follow_semver_precedence() {
        // The example chain from semver §11.4.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn malformed_versions_say_why() {
        for (text, error) in [
            ("", VersionError::Length),
            ("1", VersionError::Components),
            ("1.2.3.4.5", VersionError::Components),
            ("1..0", VersionError::Number),
            ("01.0", VersionError::Number),
            ("1.0.x", VersionError::Number),
            ("65536.0", VersionError::Number),
            ("1.0.-1", VersionError::Number),
            (" 1.0", VersionError::Number),
            ("1.0-", VersionError::Prerelease),
            ("1.0-rc..1", VersionError::Prerelease),
            ("1.0-rc_1", VersionError::Prerelease),
            ("1.0-01", VersionError::Prerelease),
            ("1.0+build", VersionError::Number),
        ] {
            assert_eq!(Version::parse(text).map(|_| ()), Err(error), "{text:?}");
        }
        let long = alloc::format!("1.0-{}", "a".repeat(MAX_VERSION_LEN));
        assert_eq!(Version::parse(&long).map(|_| ()), Err(VersionError::Length));
    }

    #[test]
    fn hyphens_inside_the_prerelease_are_identifier_bytes() {
        assert_eq!(v("1.0.0-rc-1").prerelease(), Some("rc-1"));
        assert_eq!(v("65535.65535.65535.65535").core(), [65535; 4]);
    }
}
