//! The `manifest.toml` document.
//!
//! Parsing is deliberately strict (`#[serde(deny_unknown_fields)]`) so a typo
//! is an error rather than a silently ignored field. The types here only carry
//! the document; semantic rules live in [`crate::validate`], which returns
//! every problem instead of stopping at the first.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use serde::Deserialize;

use crate::error::{ManifestError, Problem};

/// The parsed manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub app: App,
    pub entry: Entry,
    #[serde(default)]
    pub mime: Vec<MimeHandler>,
    #[serde(default)]
    pub permissions: Permissions,
}

/// `[app]`: identity and display metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub name: String,
    pub system_name: String,
    pub author: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `[entry]`: the program to run.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub binary: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// `[[mime]]`: one handled file type.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MimeHandler {
    #[serde(rename = "type")]
    pub mime_type: String,
    pub verbs: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

/// `[permissions]`: the capabilities the app asks for.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub interfaces: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub network: Vec<String>,
}

/// Parse the manifest text, tolerating a leading UTF-8 BOM. A TOML syntax
/// error, an unknown field, or a missing field becomes a single [`Problem`].
pub(crate) fn parse(text: &str) -> Result<Manifest, ManifestError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    toml::from_str(text).map_err(|error| ManifestError::new(vec![problem(format!("{error}"))]))
}

/// Collapse a multi-line parser error into one installer-friendly line.
fn problem(message: String) -> Problem {
    let mut one_line = String::with_capacity(message.len());
    for word in message.split_whitespace() {
        if !one_line.is_empty() {
            one_line.push(' ');
        }
        one_line.push_str(word);
    }
    Problem::new(one_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    const MINIMAL: &str = "\
[app]
name = \"Demo\"
system_name = \"org.lazy.demo\"
author = \"Tester\"
version = \"1.0.0\"

[entry]
binary = \"bin/app.elf\"
";

    #[test]
    fn parses_a_minimal_manifest() {
        let manifest = parse(MINIMAL).expect("valid");
        assert_eq!(manifest.app.name, "Demo");
        assert_eq!(manifest.entry.binary, "bin/app.elf");
        assert!(manifest.mime.is_empty());
        assert!(manifest.permissions.network.is_empty());
    }

    #[test]
    fn tolerates_crlf_and_a_bom() {
        let crlf = MINIMAL.replace('\n', "\r\n");
        assert!(parse(&crlf).is_ok());
        let bom = format!("\u{feff}{MINIMAL}");
        assert!(parse(&bom).is_ok());
    }

    #[test]
    fn empty_and_invalid_documents_are_errors_not_panics() {
        assert!(parse("").is_err());
        assert!(parse("this is not toml = = =").is_err());
        assert!(parse("[app]\nname = 3\n").is_err());
    }

    #[test]
    fn unknown_and_missing_fields_are_rejected() {
        let unknown = MINIMAL.replace(
            "author = \"Tester\"",
            "author = \"Tester\"\ncolour = \"red\"",
        );
        assert!(parse(&unknown).is_err());
        let missing = MINIMAL.replace("version = \"1.0.0\"\n", "");
        assert!(parse(&missing).is_err());
    }
}
