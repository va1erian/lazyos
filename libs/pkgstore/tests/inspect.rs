//! `pkgstore::inspect::assess`, the assessment `pkgd`'s `Inspect` and
//! `Install` share and the LazyRAD IDE runs in its own process (it is a
//! labelled caller, which `pkgd` refuses).

#[path = "provisioning/lzp.rs"]
#[allow(dead_code)]
mod lzp;

use lazyos_crypto::hex;
use lazypkg::Package;
use pkgstore::inspect::assess;

#[test]
fn a_valid_package_is_described_with_its_digest_and_permissions() {
    let bytes = lzp::package("os.lazy.demo", "1.2.3", 4096, 7);
    let assessed = assess(&bytes).ok().expect("the package opens");
    let info = &assessed.info;
    assert_eq!(info.system_name, "os.lazy.demo");
    assert_eq!(info.version, "1.2.3");
    assert!(info.problems.is_empty(), "{:?}", info.problems);
    // The digest the install events carry: the whole archive's SHA-256.
    let digest = Package::open(&bytes).unwrap().digest();
    assert_eq!(info.digest, hex::encode(&digest));
    assert!(info.install_dir.starts_with("os.lazy.demo/1.2.3-"));
}

#[test]
fn an_archive_that_does_not_open_is_only_its_problems() {
    let info = assess(b"PK-not-an-archive").err().expect("refused");
    assert!(!info.problems.is_empty());
    assert!(info.system_name.is_empty() && info.digest.is_empty());
}

#[test]
fn problems_are_capped_and_each_line_is_bounded() {
    let mut bytes = lzp::package("os.lazy.demo", "1.2.3", 64, 1);
    bytes.truncate(bytes.len() / 2);
    let info = assess(&bytes)
        .err()
        .expect("a truncated archive is refused");
    assert!(info.problems.len() <= 13);
    assert!(info.problems.iter().all(|p| p.chars().count() <= 200));
}

/// `(kind, value)` of every permission `Inspect` lists for a package whose
/// manifest ends with `extra`, and its problems.
fn listed(extra: &str) -> (Vec<(String, String)>, Vec<String>) {
    let bytes = lzp::package_with("os.lazy.demo", "1.0.0", 256, 3, extra);
    let Ok(assessed) = assess(&bytes) else {
        panic!("the package opens")
    };
    let info = assessed.info;
    let permissions = info
        .permissions
        .into_iter()
        .map(|p| (p.kind, p.value))
        .collect();
    (permissions, info.problems)
}

fn pair(kind: &str, value: &str) -> (String, String) {
    (kind.into(), value.into())
}

#[test]
fn a_resident_package_lists_what_resident_implies() {
    let (permissions, problems) = listed("resident = true\n");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        permissions,
        [
            pair("interface", "os.lazy.shell.tray.v1"),
            pair("interface", "os.lazy.init.app.v1"),
            pair("topic", "subscribe:session/+/shell/tray"),
            pair("resident", "true"),
        ]
    );
    let (plain, _) = listed("");
    assert!(plain.is_empty(), "{plain:?}");
}

#[test]
fn the_implied_permissions_count_toward_the_limit() {
    let interfaces = |count: usize| {
        let names: Vec<String> = (0..count).map(|i| format!("\"x.y{i}.v1\"")).collect();
        format!("[permissions]\ninterfaces = [{}]\n", names.join(", "))
    };
    // 20 + 2 interfaces, the topic and the resident line: 24, the limit.
    let (permissions, problems) = listed(&format!("resident = true\n{}", interfaces(20)));
    assert_eq!(permissions.len(), 24);
    assert!(problems.is_empty(), "{problems:?}");
    // One more is refused, though the manifest itself names only 21.
    let (permissions, problems) = listed(&format!("resident = true\n{}", interfaces(21)));
    assert_eq!(permissions.len(), 25);
    assert_eq!(
        problems,
        ["the package requests 25 permissions; at most 24 are allowed"]
    );
    let (_, problems) = listed(&interfaces(21));
    assert!(problems.is_empty(), "{problems:?}");
}
