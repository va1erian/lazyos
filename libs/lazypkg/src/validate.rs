//! Semantic validation of a parsed manifest.
//!
//! [`validate`] returns *every* problem it finds, so the installer can show a
//! complete list rather than making the author fix one thing at a time. The
//! grammar mirrors `docs/packages.md`; the Python builder checks the same
//! rules before an archive is ever written.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Problem;
use crate::manifest::Manifest;

/// Longest `system_name`, in bytes.
const MAX_SYSTEM_NAME: usize = 128;
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
    if !valid_version(&app.version) {
        problems.push(Problem::new(format!(
            "app.version {:?} must be MAJOR.MINOR.PATCH with parts below 65536",
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
}

fn check_entry(manifest: &Manifest, files: &BTreeSet<&str>, problems: &mut Vec<Problem>) {
    let binary = &manifest.entry.binary;
    if !binary.ends_with(".elf") {
        problems.push(Problem::new(format!(
            "entry.binary {binary:?} must name a .elf file"
        )));
    } else if !files.contains(binary.as_str()) {
        problems.push(Problem::new(format!(
            "entry.binary {binary:?} is missing from the package"
        )));
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
    files: &BTreeSet<&str>,
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
                if !files.contains(path.as_str()) {
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
        if !valid_file_rule(rule) {
            problems.push(Problem::new(format!(
                "permissions.files entry {rule:?} is not a read:/write: absolute path"
            )));
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

/// `[a-z0-9]` labels joined by `.`, at least three, none starting or ending in
/// `-`, at most 128 bytes.
fn valid_system_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_SYSTEM_NAME {
        return false;
    }
    let mut labels = 0;
    for label in name.split('.') {
        labels += 1;
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return false;
        }
    }
    labels >= 3
}

/// `MAJOR.MINOR.PATCH`, each an unsigned decimal below 65536.
fn valid_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let major = parts.next();
    let minor = parts.next();
    let patch = parts.next();
    if parts.next().is_some() {
        return false;
    }
    [major, minor, patch]
        .into_iter()
        .all(|part| part.is_some_and(valid_component))
}

fn valid_component(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit())
        && part.parse::<u64>().is_ok_and(|value| value < 65536)
}

/// `type/subtype` over `[a-z0-9.+-]`.
fn valid_mime_type(mime: &str) -> bool {
    let mut parts = mime.split('/');
    let (Some(kind), Some(subtype), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && kind.bytes().all(is_mime_byte)
        && subtype.bytes().all(is_mime_byte)
}

fn is_mime_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-')
}

/// `[a-z0-9]+(\.[a-z0-9]+)*\.v[0-9]+`.
fn valid_interface(interface: &str) -> bool {
    let mut parts: Vec<&str> = interface.split('.').collect();
    let Some(version) = parts.pop() else {
        return false;
    };
    if parts.is_empty() {
        return false;
    }
    if !version.starts_with('v') || version.len() < 2 {
        return false;
    }
    if !version[1..].bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// `publish:`/`subscribe:` then `/`-separated segments of `[a-z0-9_.-]+`, `+`,
/// or a final `#`.
fn valid_topic(topic: &str) -> bool {
    let rest = topic
        .strip_prefix("publish:")
        .or_else(|| topic.strip_prefix("subscribe:"));
    let Some(rest) = rest else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    let last = segments.len() - 1;
    segments.iter().enumerate().all(|(index, segment)| {
        if segment.is_empty() {
            return false;
        }
        if *segment == "#" {
            return index == last;
        }
        if *segment == "+" {
            return true;
        }
        segment.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
    })
}

/// `read:`/`write:` then an absolute path of `[A-Za-z0-9_.-]+` or `*`
/// segments, with no `..`.
fn valid_file_rule(rule: &str) -> bool {
    let rest = rule
        .strip_prefix("read:")
        .or_else(|| rule.strip_prefix("write:"));
    let Some(rest) = rest else {
        return false;
    };
    let Some(path) = rest.strip_prefix('/') else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    path.split('/').all(|segment| {
        if segment.is_empty() || segment == ".." {
            return false;
        }
        segment == "*"
            || segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    })
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
        for version in ["1.0", "1.0.0.0", "1.0.x", "65536.0.0", "1.0.-1", ""] {
            let mut bad = minimal();
            bad.app.version = version.into();
            let problems = validate(&bad, &files(&["bin/app.elf"]));
            assert!(
                problems.iter().any(|p| p.message().contains("version")),
                "{version:?} should be rejected"
            );
        }
        let mut ok = minimal();
        ok.app.version = "65535.65535.65535".into();
        assert!(validate(&ok, &files(&["bin/app.elf"])).is_empty());
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
            "read:/data/home/*/pictures".into(),
            "write:/data/home/*/pictures".into(),
        ];
        good.permissions.network = vec!["outbound".into()];
        assert!(validate(&good, &files(&["bin/app.elf"])).is_empty());
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
