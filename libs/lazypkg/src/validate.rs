//! Semantic validation of a parsed manifest.
//!
//! [`validate`] returns *every* problem it finds, so the installer can show a
//! complete list rather than making the author fix one thing at a time. The
//! grammar mirrors `docs/packages.md`; the Python builder
//! (`tools/pkg/pkgmanifest.py`) checks the same rules before an archive is ever
//! written, and both are run against the shared cases in
//! `libs/lazypkg/tests/cases/manifest.toml`.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Problem;
use crate::files;
use crate::grammar::{valid_interface, valid_mime_type, valid_system_name, valid_topic};
use crate::manifest::{Category, Manifest};
use crate::version::Version;

/// Longest display name and author, in characters.
const MAX_NAME: usize = 64;
const MAX_AUTHOR: usize = 128;
/// Longest description, in characters.
const MAX_DESCRIPTION: usize = 1024;
/// Limits on `entry.args`.
const MAX_ARGS: usize = 16;
const MAX_ARG: usize = 256;
/// Longest verb in a `[[mime]]` block.
const MAX_VERB: usize = 16;

/// Validate the manifest against the file names in the archive.
pub(crate) fn validate(manifest: &Manifest, files: &BTreeSet<&str>) -> Vec<Problem> {
    validate_with(manifest, Some(files))
}

/// Validate everything the manifest says on its own: no archive to check file
/// names against, so the existence of `entry.binary` and the MIME icons is not
/// judged. This is what a manifest read back from an install directory gets.
pub(crate) fn validate_standalone(manifest: &Manifest) -> Vec<Problem> {
    validate_with(manifest, None)
}

fn validate_with(manifest: &Manifest, files: Option<&BTreeSet<&str>>) -> Vec<Problem> {
    let mut problems = Vec::new();
    check_app(manifest, &mut problems);
    check_entry(manifest, files, &mut problems);
    for (index, mime) in manifest.mime.iter().enumerate() {
        check_mime(index, mime, files, &mut problems);
    }
    check_permissions(manifest, &mut problems);
    problems
}

fn check_app(manifest: &Manifest, problems: &mut Vec<Problem>) {
    let app = &manifest.app;
    if !(1..=MAX_NAME).contains(&app.name.chars().count()) {
        problems.push(Problem::new(format!(
            "app.name must be 1..={MAX_NAME} characters"
        )));
    }
    if app.name.chars().any(char::is_control) {
        problems.push(Problem::new(String::from(
            "app.name must not contain control characters",
        )));
    }
    if !valid_system_name(&app.system_name) {
        problems.push(Problem::new(format!(
            "app.system_name {:?} is not a reverse-DNS name",
            app.system_name
        )));
    }
    if !(1..=MAX_AUTHOR).contains(&app.author.chars().count()) {
        problems.push(Problem::new(format!(
            "app.author must be 1..={MAX_AUTHOR} characters"
        )));
    }
    if let Err(error) = Version::parse(&app.version) {
        problems.push(Problem::new(format!(
            "app.version {:?} {error}",
            app.version
        )));
    }
    if let Some(description) = &app.description {
        if description.chars().count() > MAX_DESCRIPTION {
            problems.push(Problem::new(format!(
                "app.description must be at most {MAX_DESCRIPTION} characters"
            )));
        }
    }
    if let Some(category) = &app.category {
        if Category::parse(category).is_none() {
            let names: Vec<&str> = Category::ALL.iter().map(|c| c.as_str()).collect();
            problems.push(Problem::new(format!(
                "app.category {category:?} must be one of {}",
                names.join(", ")
            )));
        }
    }
}

fn check_entry(manifest: &Manifest, files: Option<&BTreeSet<&str>>, problems: &mut Vec<Problem>) {
    let binary = &manifest.entry.binary;
    if !binary.ends_with(".elf") {
        problems.push(Problem::new(format!(
            "entry.binary {binary:?} must name a .elf file"
        )));
    } else if files.is_some_and(|files| !files.contains(binary.as_str())) {
        problems.push(Problem::new(format!(
            "entry.binary {binary:?} is missing from the package"
        )));
    }
    if let Some(abi) = &manifest.entry.abi {
        if abi != "native" && abi != "linux" {
            problems.push(Problem::new(format!(
                "entry.abi {abi:?} must be \"native\" or \"linux\""
            )));
        }
    }
    if manifest.entry.args.len() > MAX_ARGS {
        problems.push(Problem::new(format!(
            "entry.args may hold at most {MAX_ARGS} items"
        )));
    }
    for arg in &manifest.entry.args {
        if arg.len() > MAX_ARG {
            problems.push(Problem::new(format!(
                "entry.args items must be at most {MAX_ARG} bytes"
            )));
        }
    }
}

fn check_mime(
    index: usize,
    mime: &crate::manifest::MimeHandler,
    files: Option<&BTreeSet<&str>>,
    problems: &mut Vec<Problem>,
) {
    if !valid_mime_type(&mime.mime_type) {
        problems.push(Problem::new(format!(
            "mime[{index}].type {:?} is not a type/subtype",
            mime.mime_type
        )));
    }
    if mime.verbs.is_empty() {
        problems.push(Problem::new(format!(
            "mime[{index}].verbs must not be empty"
        )));
    }
    for verb in &mime.verbs {
        if verb.is_empty() || verb.len() > MAX_VERB || !verb.bytes().all(|b| b.is_ascii_lowercase())
        {
            problems.push(Problem::new(format!(
                "mime[{index}] verb {verb:?} must be 1..={MAX_VERB} lowercase letters"
            )));
        }
    }
    if let Some(prefix) = &mime.icon {
        if !prefix.starts_with("icons/") || prefix.contains("..") {
            problems.push(Problem::new(format!(
                "mime[{index}].icon {prefix:?} must be an icons/ prefix"
            )));
        } else {
            for size in ["16", "32", "128"] {
                let path = format!("{prefix}-{size}.png");
                if files.is_some_and(|files| !files.contains(path.as_str())) {
                    problems.push(Problem::new(format!(
                        "mime[{index}].icon {path:?} is missing from the package"
                    )));
                }
            }
        }
    }
}

fn check_permissions(manifest: &Manifest, problems: &mut Vec<Problem>) {
    let permissions = &manifest.permissions;
    for interface in &permissions.interfaces {
        if !valid_interface(interface) {
            problems.push(Problem::new(format!(
                "permissions.interfaces entry {interface:?} is not name.vN"
            )));
        }
    }
    for topic in &permissions.topics {
        if !valid_topic(topic) {
            problems.push(Problem::new(format!(
                "permissions.topics entry {topic:?} is not a publish:/subscribe: pattern"
            )));
        }
    }
    for rule in &permissions.files {
        if let Err(error) = files::check_rule(rule) {
            problems.push(Problem::new(error.message(rule)));
        }
    }
    if !permissions.network.is_empty()
        && !(permissions.network.len() == 1 && permissions.network[0] == "outbound")
    {
        problems.push(Problem::new(String::from(
            "permissions.network must be empty or exactly [\"outbound\"]",
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;
    use alloc::vec;

    fn minimal() -> Manifest {
        manifest::parse(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\n[entry]\nbinary = \"bin/app.elf\"\n",
        )
        .expect("valid")
    }

    fn files(names: &[&'static str]) -> BTreeSet<&'static str> {
        names.iter().copied().collect()
    }

    #[test]
    fn a_good_manifest_has_no_problems() {
        let mut good = minimal();
        good.mime.push(manifest::MimeHandler {
            mime_type: "image/png".into(),
            verbs: vec!["open".into()],
            icon: None,
        });
        good.permissions
            .interfaces
            .push("os.lazy.clipboard.v1".into());
        good.permissions
            .topics
            .push("publish:app/org.lazy.demo/#".into());
        good.permissions.files.push("read:/data/home/*".into());
        good.permissions
            .files
            .push("write:$HOME/Documents/*".into());
        good.app.category = Some("office".into());
        good.entry.autostart = true;
        let problems = validate(&good, &files(&["bin/app.elf"]));
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn rejects_bad_system_names() {
        for name in [
            "org.lazy",      // too few labels
            "Org.Lazy.Demo", // uppercase
            "org..demo",     // empty label
            "org.lazy.-x",   // leading hyphen
            "org.lazy.x-",   // trailing hyphen
            "org.lazy.d mo", // space
        ] {
            let mut bad = minimal();
            bad.app.system_name = name.into();
            let problems = validate(&bad, &files(&["bin/app.elf"]));
            assert!(
                problems.iter().any(|p| p.message().contains("system_name")),
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_bad_versions() {
        for version in [
            "1",
            "1.0.0.0.0",
            "1.0.x",
            "65536.0.0",
            "1.0.-1",
            "",
            "01.0.0",
            "1.0.0-",
        ] {
            let mut bad = minimal();
            bad.app.version = version.into();
            let problems = validate(&bad, &files(&["bin/app.elf"]));
            assert!(
                problems.iter().any(|p| p.message().contains("app.version")),
                "{version:?} should be rejected"
            );
        }
        for version in ["65535.65535.65535", "1.0", "1.0.0.0", "1.0.0-rc1"] {
            let mut ok = minimal();
            ok.app.version = version.into();
            assert!(
                validate(&ok, &files(&["bin/app.elf"])).is_empty(),
                "{version}"
            );
        }
    }

    #[test]
    fn rejects_an_unknown_category() {
        let mut bad = minimal();
        bad.app.category = Some("games".into());
        let problems = validate(&bad, &files(&["bin/app.elf"]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message().contains("app.category \"games\""));
        assert!(problems[0].message().contains("accessories, development"));
    }

    #[test]
    fn rejects_bad_interfaces_topics_and_files() {
        let mut bad = minimal();
        bad.permissions.interfaces = vec![
            "os.lazy.clipboard".into(),
            "os.lazy.clipboard.v".into(),
            "os.lazy.clipboard.vX".into(),
            "os.lazy-clipboard.v1".into(),
        ];
        bad.permissions.topics = vec![
            "app/x".into(),
            "publish:".into(),
            "publish:a/#/b".into(),
            "publish:A".into(),
        ];
        bad.permissions.files = vec![
            "read:relative".into(),
            "write:/a/../b".into(),
            "read:/".into(),
            "read:/a//b".into(),
        ];
        let problems = validate(&bad, &files(&["bin/app.elf"]));
        assert_eq!(problems.len(), 12, "{problems:?}");
    }

    #[test]
    fn accepts_the_documented_examples() {
        let mut good = minimal();
        good.permissions.interfaces =
            vec!["os.lazy.clipboard.v1".into(), "os.lazy.fs.reader.v1".into()];
        good.permissions.topics = vec![
            "publish:app/org.lazy.paint/#".into(),
            "subscribe:system/events/open/+".into(),
        ];
        good.permissions.files = vec![
            "read:$HOME/Pictures/*".into(),
            "write:$HOME/.apps/org.lazy.paint/*".into(),
        ];
        good.permissions.network = vec!["outbound".into()];
        assert!(validate(&good, &files(&["bin/app.elf"])).is_empty());
    }

    #[test]
    fn abi_must_be_native_or_linux() {
        let mut m = minimal();
        assert!(validate(&m, &files(&["bin/app.elf"])).is_empty());
        for abi in ["native", "linux"] {
            m.entry.abi = Some(abi.into());
            assert!(validate(&m, &files(&["bin/app.elf"])).is_empty(), "{abi}");
        }
        m.entry.abi = Some("windows".into());
        let problems = validate(&m, &files(&["bin/app.elf"]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message().contains("entry.abi"));
        assert!(!minimal().entry.is_linux());
        m.entry.abi = Some("linux".into());
        assert!(m.entry.is_linux());
    }

    #[test]
    fn standalone_validation_skips_file_existence_only() {
        let mut m = minimal();
        m.mime.push(manifest::MimeHandler {
            mime_type: "image/png".into(),
            verbs: vec!["open".into()],
            icon: Some("icons/png".into()),
        });
        assert!(validate_standalone(&m).is_empty());
        m.entry.binary = "bin/app.exe".into();
        m.app.version = "x".into();
        assert_eq!(validate_standalone(&m).len(), 2);
    }

    #[test]
    fn rejects_a_missing_binary_or_icon() {
        let mut bad = minimal();
        bad.entry.binary = "bin/missing.elf".into();
        bad.mime.push(manifest::MimeHandler {
            mime_type: "image/png".into(),
            verbs: vec!["open".into()],
            icon: Some("icons/png".into()),
        });
        let problems = validate(&bad, &files(&["bin/app.elf"]));
        assert_eq!(problems.len(), 4, "{problems:?}"); // binary + three icons
    }
}
