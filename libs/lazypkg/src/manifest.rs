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
use crate::version::Version;

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
    /// The menu group: one of [`Category::ALL`]'s names. Absent means
    /// [`Category::Accessories`]; read it through [`App::category`].
    #[serde(default)]
    pub category: Option<String>,
}

impl App {
    /// The menu category. A validated manifest always names a known one; an
    /// absent (or, in an unvalidated manifest, unknown) one is the default.
    pub fn category(&self) -> Category {
        self.category
            .as_deref()
            .and_then(Category::parse)
            .unwrap_or_default()
    }

    /// `version` as a [`Version`]; `None` only for an unvalidated manifest.
    pub fn parsed_version(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }
}

/// The menu group an app is listed under (`[app] category`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    #[default]
    Accessories,
    Development,
    Games,
    Graphics,
    Internet,
    Office,
    System,
    Utilities,
}

impl Category {
    /// Every category, in menu order.
    pub const ALL: [Category; 8] = [
        Category::Accessories,
        Category::Development,
        Category::Games,
        Category::Graphics,
        Category::Internet,
        Category::Office,
        Category::System,
        Category::Utilities,
    ];

    /// The manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Accessories => "accessories",
            Category::Development => "development",
            Category::Games => "games",
            Category::Graphics => "graphics",
            Category::Internet => "internet",
            Category::Office => "office",
            Category::System => "system",
            Category::Utilities => "utilities",
        }
    }

    /// The category spelled `name`, exactly (lowercase).
    pub fn parse(name: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

/// `[entry]`: the program to run.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub binary: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The program's ABI: `native` (the default, a LazyOS program) or `linux`
    /// (a static musl program the Linux ABI personality runs). An ELF header
    /// cannot tell the two apart, so the package says which one it is.
    #[serde(default)]
    pub abi: Option<String>,
    /// Start the app when the user logs in. For a user package this is shown
    /// at consent and honoured only after it.
    #[serde(default)]
    pub autostart: bool,
}

impl Entry {
    /// Whether the program runs under the Linux ABI personality.
    pub fn is_linux(&self) -> bool {
        self.abi.as_deref() == Some("linux")
    }
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
    /// `develop = true`: the app may run apps the user is developing under
    /// their own approved permissions (an IDE's Play, issue #529). It compiles
    /// to the kernel rule that lets the app spawn a child into a `dev:` label
    /// (`os.lazy.process.label.spawn.v1`); `pkgd` still asks the user before
    /// each development label gets its rules.
    #[serde(default)]
    pub develop: bool,
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
        assert_eq!(manifest.app.category(), Category::Accessories);
        assert!(!manifest.entry.autostart);
        assert_eq!(manifest.app.parsed_version(), Version::parse("1.0").ok());
    }

    #[test]
    fn category_and_autostart_parse() {
        let text = MINIMAL.replace(
            "version = \"1.0.0\"",
            "version = \"1.0.0\"\ncategory = \"graphics\"",
        ) + "autostart = true\n";
        let manifest = parse(&text).expect("valid");
        assert_eq!(manifest.app.category(), Category::Graphics);
        assert!(manifest.entry.autostart);
        assert!(parse(&format!("{MINIMAL}autostart = \"yes\"\n")).is_err());
        for category in Category::ALL {
            assert_eq!(Category::parse(category.as_str()), Some(category));
        }
        assert_eq!(Category::parse("Graphics"), None);
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
